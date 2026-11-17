use super::*;

enum BatchStep<'a> {
    Ready {
        call: &'a ToolCall,
        context: ToolContext,
        permit: Arc<crate::approvals::OneShotPermit>,
        buffered_store: Arc<BufferedReadStore>,
    },
    ImmediateLocal {
        call: &'a ToolCall,
        result: ToolResult,
    },
}

impl AgentRuntime {
    pub(super) async fn execute_parallel_read_batch(
        &self,
        session: &mut Session,
        calls: &[ToolCall],
        previous: &mut Option<(String, serde_json::Value, u8)>,
    ) -> Result<(usize, bool), BoundaryError> {
        let steps = self.prepare_parallel_read_steps(session, calls).await?;
        if steps.is_empty() {
            return Ok((0, false));
        }

        let outcomes = self
            .run_parallel_read_futures(session, calls, &steps)
            .await?;
        self.commit_parallel_read_steps(session, calls, steps, outcomes, previous)
            .await
    }

    async fn prepare_parallel_read_steps<'a>(
        &self,
        session: &mut Session,
        calls: &'a [ToolCall],
    ) -> Result<Vec<BatchStep<'a>>, BoundaryError> {
        let snap = self.wait_for_mode_ready(session).await?;
        let context = self.context(session, snap.current);
        let max_batch = calls.len().min(8);
        let mut steps: Vec<BatchStep<'a>> = Vec::with_capacity(max_batch);

        for (offset, call) in calls[..max_batch].iter().enumerate() {
            if !is_parallel_read_tool_name(&call.name)
                || !self.dispatcher.is_parallel_read_preapproved(call, &context)
            {
                break;
            }
            if let Err(err) = self.check() {
                if steps.is_empty() {
                    return Ok(Vec::new());
                }
                let _ = err;
                break;
            }
            if let Some(state) = &self.project_context {
                if let Some(target) = context_target(call) {
                    state
                        .lock()
                        .map_err(|_| {
                            BoundaryError::new(
                                error_codes::PROJECT_CONTEXT_ERROR,
                                "project context lock failed",
                            )
                        })?
                        .target = Some(target);
                }
                if !project_context_is_current(state, call)? {
                    if steps.is_empty() {
                        return Ok(Vec::new());
                    }
                    break;
                }
            }
            if let Err(failure) = self.services.reserve_tool(self.limits.tool_calls) {
                if steps.is_empty() {
                    self.close_calls(session, &calls[offset..], CallProgress::NotDispatched)
                        .await?;
                    return Err(failure);
                }
                break;
            }
            self.metrics.tool_call();
            if let Some(blocked) = match self.check_pre_dispatch_hooks(session, call).await {
                Ok(val) => val,
                Err(err) => {
                    self.close_calls(session, &calls[offset..], CallProgress::NotDispatched)
                        .await?;
                    return Err(err);
                }
            } {
                steps.push(BatchStep::ImmediateLocal {
                    call,
                    result: blocked,
                });
                continue;
            }

            let review = DispatchReview {
                model: self.model.as_ref(),
                budget: &self.budget,
                config: &self.review,
            };
            match self
                .dispatcher
                .authorize(
                    call,
                    &context,
                    &self.summary,
                    self.confirmation.is_some(),
                    review,
                )
                .await
            {
                Ok(Authorization::Permit(permit)) => {
                    steps.push(BatchStep::Ready {
                        call,
                        context: context.clone(),
                        permit,
                        buffered_store: Arc::new(BufferedReadStore::new(self.store.clone())),
                    });
                }
                Ok(Authorization::Stale) => {
                    steps.push(BatchStep::ImmediateLocal {
                        call,
                        result: ToolResult::failure(
                            error_codes::STALE_APPROVAL,
                            "mode or permission changed",
                        ),
                    });
                    break;
                }
                Ok(Authorization::Denied(_)) => {
                    steps.push(BatchStep::ImmediateLocal {
                        call,
                        result: ToolResult::failure(
                            error_codes::PERMISSION_DENIED,
                            "operation denied",
                        ),
                    });
                    break;
                }
                Ok(Authorization::NeedsConfirmation(_) | Authorization::Pending(_)) => {
                    steps.push(BatchStep::ImmediateLocal {
                        call,
                        result: ToolResult::failure(
                            error_codes::NEEDS_CONFIRMATION,
                            "confirmation required",
                        ),
                    });
                    break;
                }
                Err(e) if invalid_tool_request(&e) => {
                    steps.push(BatchStep::ImmediateLocal {
                        call,
                        result: ToolResult::failure(e.code(), e.message()),
                    });
                }
                Err(e) => {
                    self.close_calls(session, &calls[offset..], CallProgress::NotDispatched)
                        .await?;
                    return Err(e);
                }
            }
        }

        Ok(steps)
    }

    async fn run_parallel_read_futures(
        &self,
        session: &mut Session,
        calls: &[ToolCall],
        steps: &[BatchStep<'_>],
    ) -> Result<VecDeque<Result<ToolResult, BoundaryError>>, BoundaryError> {
        let ready_indices: Vec<usize> = steps
            .iter()
            .enumerate()
            .filter_map(|(idx, step)| matches!(step, BatchStep::Ready { .. }).then_some(idx))
            .collect();
        if ready_indices.is_empty() {
            return Ok(VecDeque::new());
        }

        let batch_cancel = CallCancel::new(self.cancel.clone());
        let operation = futures::future::join_all(ready_indices.iter().map(|&idx| {
            let BatchStep::Ready {
                call,
                context,
                permit,
                buffered_store,
            } = &steps[idx]
            else {
                unreachable!();
            };
            let store: Arc<dyn Store> = buffered_store.clone();
            self.dispatcher.execute_with_cancel_on_store(
                call,
                context,
                permit.clone(),
                &batch_cancel,
                false,
                store,
            )
        }));
        tokio::pin!(operation);
        let mut interrupted: Option<BoundaryError> = None;
        let completed = tokio::select! {
            biased;
            () = self.cancel.cancelled() => {
                interrupted = Some(BoundaryError::new(error_codes::CANCELLED, "run cancelled"));
                None
            }
            () = sleep(self.budget.remaining()?) => {
                interrupted = Some(BoundaryError::new(
                    error_codes::LIMIT_REACHED,
                    "active time budget exceeded",
                ));
                None
            }
            () = sleep(self.limits.tool_timeout) => {
                interrupted = Some(BoundaryError::new(error_codes::TOOL_TIMEOUT, "tool timed out"));
                None
            }
            results = &mut operation => Some(results),
        };
        if let Some(results) = completed {
            return Ok(results.into());
        }

        batch_cancel.stop();
        let settled = operation.await;
        self.recover_interrupted_parallel_read_batch(session, calls, steps, settled, interrupted)
            .await
    }

    async fn recover_interrupted_parallel_read_batch(
        &self,
        session: &mut Session,
        calls: &[ToolCall],
        steps: &[BatchStep<'_>],
        settled: Vec<Result<ToolResult, BoundaryError>>,
        interrupted: Option<BoundaryError>,
    ) -> Result<VecDeque<Result<ToolResult, BoundaryError>>, BoundaryError> {
        let mut settled_iter = settled.into_iter();
        for (offset, step) in steps.iter().enumerate() {
            match step {
                BatchStep::ImmediateLocal { call, result } => {
                    let _ = self.save_local(session, call, result.clone()).await?;
                }
                BatchStep::Ready {
                    call,
                    buffered_store,
                    ..
                } => match settled_iter.next() {
                    Some(Ok(result)) => {
                        let changes = buffered_store.take_changes()?;
                        if !changes.is_empty() {
                            self.store.commit(&changes).await?;
                        }
                        session.history.push(HistoryItem::ToolResult {
                            call_id: call.call_id.clone(),
                            result,
                        });
                    }
                    _ => {
                        if let Some(saved) = self.store.load_session(session.id).await? {
                            if saved.history.len() < session.history.len() {
                                return Err(BoundaryError::new(
                                    error_codes::STORAGE_ERROR,
                                    "saved session history became shorter during tool cleanup",
                                ));
                            }
                            session.history = saved.history;
                        }
                        self.close_calls(session, &calls[offset..], CallProgress::Dispatched)
                            .await?;
                        return Err(interrupted.unwrap_or_else(|| {
                            BoundaryError::new(error_codes::CANCELLED, "run interrupted")
                        }));
                    }
                },
            }
        }
        Err(interrupted
            .unwrap_or_else(|| BoundaryError::new(error_codes::CANCELLED, "run interrupted")))
    }

    async fn commit_parallel_read_steps(
        &self,
        session: &mut Session,
        calls: &[ToolCall],
        steps: Vec<BatchStep<'_>>,
        mut outcomes: VecDeque<Result<ToolResult, BoundaryError>>,
        previous: &mut Option<(String, serde_json::Value, u8)>,
    ) -> Result<(usize, bool), BoundaryError> {
        let consumed = steps.len();
        for (offset, step) in steps.into_iter().enumerate() {
            let (call, result) = match step {
                BatchStep::ImmediateLocal { call, result } => {
                    (call, self.save_local(session, call, result).await?)
                }
                BatchStep::Ready {
                    call,
                    buffered_store,
                    ..
                } => {
                    let outcome = outcomes.pop_front().ok_or_else(|| {
                        BoundaryError::new(error_codes::RUNTIME_ERROR, "missing batch outcome")
                    })?;
                    match outcome {
                        Ok(result) => {
                            let changes = buffered_store.take_changes()?;
                            if !changes.is_empty()
                                && let Err(mut err) = self.store.commit(&changes).await
                            {
                                err.set_observed_result(Some(result));
                                return Err(err);
                            }
                            session.history.push(HistoryItem::ToolResult {
                                call_id: call.call_id.clone(),
                                result: result.clone(),
                            });
                            (call, result)
                        }
                        Err(err) if err.is_code(error_codes::PROJECT_CONTEXT_CHANGED) => {
                            let res = self
                                .save_local(
                                    session,
                                    call,
                                    ToolResult::failure(
                                        error_codes::PROJECT_CONTEXT_CHANGED,
                                        "project context changed before dispatch",
                                    ),
                                )
                                .await?;
                            (call, res)
                        }
                        Err(err) => {
                            if !err.is_code(error_codes::STORAGE_ERROR)
                                && !err.has_observed_result()
                            {
                                self.close_calls(
                                    session,
                                    &calls[offset..],
                                    CallProgress::Dispatched,
                                )
                                .await?;
                            }
                            return Err(err);
                        }
                    }
                }
            };
            if self
                .post_tool_call_checks(session, call, &result, &calls[offset + 1..], previous)
                .await?
            {
                return Ok((offset + 1, true));
            }
        }
        Ok((consumed, false))
    }
}
