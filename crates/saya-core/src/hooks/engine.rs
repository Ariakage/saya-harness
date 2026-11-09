use super::*;
use std::{collections::BTreeSet, sync::atomic::Ordering};

impl HookEngine {
    pub(super) async fn fire_inner(
        &self,
        context: &HookContext,
        hooks: &[EnabledHook],
        cancel: &dyn Cancellation,
    ) -> Result<HookBatchOutcome, BoundaryError> {
        self.validate_context(context)?;
        let payload = output::payload(context, self.limits.max_payload_bytes, &self.secrets)?;
        let selected: Vec<_> = hooks
            .iter()
            .filter(|hook| hook.event == context.event)
            .take(self.limits.max_hooks_per_event + 1)
            .collect();
        if selected.len() > self.limits.max_hooks_per_event {
            return Err(error("hook_budget", "Event hook count exceeds its budget"));
        }
        let mut seen = BTreeSet::new();
        for hook in &selected {
            if !seen.insert(hook.key()) {
                return Err(error("hook_config", "Event hook entries are duplicated"));
            }
            if hook.lease.record().session_id != context.run.session_id
                || hook.lease.record().run_id != context.run.id
            {
                return Err(error(
                    "hook_identity",
                    "Hook lease belongs to another run or session",
                ));
            }
            output::reject_identity_secrets(&hook.entry, &self.secrets)?;
        }
        let mut outcome = HookBatchOutcome::default();
        for hook in selected {
            if cancel.is_cancelled() {
                outcome.cancelled = true;
                outcome.blocked = true;
                break;
            }
            let intent = self.intent(context, hook, &payload)?;
            if context.depth >= self.limits.max_depth
                || context
                    .ancestry
                    .iter()
                    .any(|ancestor| ancestor.hook_key == hook.key())
            {
                self.note(
                    &intent,
                    "hook_recursion",
                    "Hook recursion was suppressed",
                    &mut outcome,
                )
                .await?;
                outcome.blocked |= hook.kind == HookKind::Check;
                continue;
            }
            let claim =
                self.journal.claim(&intent).await.map_err(|_| {
                    error("hook_journal", "Hook intent could not be durably claimed")
                })?;
            let record = match claim {
                HookClaim::Execute => {
                    let fresh = self
                        .invocations
                        .try_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                            (count < self.limits.max_invocations_per_run).then_some(count + 1)
                        })
                        .is_ok();
                    let record = if fresh {
                        self.execute(context, hook, &intent, payload.clone(), cancel)
                            .await
                    } else {
                        failed_record(
                            &intent,
                            HookStatus::Failed,
                            HookExecutionState::NotStarted,
                            CleanupState::Cleaned,
                            "hook_budget",
                            "Run hook count exceeds its budget",
                        )
                    };
                    let record = output::record(record, &self.secrets)?;
                    if self.journal.finish(&record).await.is_err() {
                        return Err(unsaved(&record));
                    }
                    record
                }
                HookClaim::Completed(record) => {
                    if record.intent != intent || !valid_record(&record) {
                        self.note(
                            &intent,
                            "hook_conflict",
                            "Stored hook result does not match this event",
                            &mut outcome,
                        )
                        .await?;
                        outcome.blocked = true;
                        continue;
                    }
                    output::record(*record, &self.secrets)?
                }
                HookClaim::Running | HookClaim::Unknown => {
                    self.note(
                        &intent,
                        "hook_unknown",
                        "Hook is running or requires independent reconciliation",
                        &mut outcome,
                    )
                    .await?;
                    outcome.blocked = true;
                    outcome.cleanup_pending = true;
                    continue;
                }
                HookClaim::Conflict => {
                    self.note(
                        &intent,
                        "hook_conflict",
                        "Hook event identity conflicts with its durable intent",
                        &mut outcome,
                    )
                    .await?;
                    outcome.blocked = true;
                    continue;
                }
                HookClaim::BudgetExhausted => {
                    self.note(
                        &intent,
                        "hook_budget",
                        "Durable run hook count exceeds its budget",
                        &mut outcome,
                    )
                    .await?;
                    outcome.blocked |= hook.kind == HookKind::Check;
                    continue;
                }
            };
            let failed = record.status != HookStatus::Succeeded;
            if hook.kind == HookKind::Check && failed {
                outcome.blocked = true;
            }
            if record.cleanup == CleanupState::Unknown
                || record.execution == HookExecutionState::Unknown
            {
                outcome.cleanup_pending = true;
                outcome.blocked = true;
            }
            if record.status == HookStatus::Cancelled {
                outcome.cancelled = true;
                outcome.blocked = true;
            }
            if failed {
                self.note(
                    &intent,
                    "hook_failure",
                    "Hook failed or denied the event; inspect its recorded status",
                    &mut outcome,
                )
                .await?;
            }
            outcome.records.push(record);
            if outcome.cancelled || outcome.cleanup_pending {
                break;
            }
        }
        Ok(outcome)
    }

    fn validate_context(&self, context: &HookContext) -> Result<(), BoundaryError> {
        let tool_event = matches!(
            context.event,
            HookEvent::ToolBeforeDispatch | HookEvent::ToolFinished
        );
        if context.run.id != self.run.id
            || context.run.session_id != self.run.session_id
            || context.run.permissions != self.run.permissions
            || !identity(&context.event_id)
            || !identity(&context.root_event_id)
            || context
                .parent_call_id
                .as_ref()
                .is_some_and(|id| !identity(id))
            || tool_event != context.parent_call_id.is_some()
            || context.depth as usize != context.ancestry.len()
            || context.depth > self.limits.max_depth
            || (context.depth == 0 && context.event_id != context.root_event_id)
            || context.ancestry.iter().any(|ancestor| {
                !is_digest(&ancestor.hook_key) || !is_digest(&ancestor.invocation_id)
            })
        {
            return Err(error(
                "hook_identity",
                "Hook host event metadata is invalid",
            ));
        }
        output::reject_identity_secrets(
            &(
                &context.event_id,
                &context.root_event_id,
                &context.parent_call_id,
                &context.run,
                &context.ancestry,
            ),
            &self.secrets,
        )
    }

    fn intent(
        &self,
        context: &HookContext,
        hook: &EnabledHook,
        payload: &Value,
    ) -> Result<HookIntent, BoundaryError> {
        let key = output::encode(
            &(
                &context.run.session_id,
                &context.run.id,
                &context.event_id,
                &context.root_event_id,
                context.event,
                &context.parent_call_id,
                hook.key(),
            ),
            4096,
        )?;
        let mut intent = HookIntent {
            invocation_id: digest(&key),
            fingerprint: String::new(),
            event_id: context.event_id.clone(),
            root_event_id: context.root_event_id.clone(),
            event: context.event,
            kind: hook.kind,
            run: context.run.clone(),
            parent_call_id: context.parent_call_id.clone(),
            depth: context.depth,
            plugin_id: hook.lease.record().plugin_id.clone(),
            package_digest: hook.lease.record().digest.clone(),
            entry_id: hook.entry.id.clone(),
            entry_sha256: hook.entry_sha256.clone(),
            lease_id: hook.lease.record().lease_id.clone(),
            enable_revision: hook.lease.record().enable_revision,
            payload_sha256: digest(&output::encode(payload, self.limits.max_payload_bytes)?),
            timeout_ms: hook.entry.timeout_ms,
            output_max_bytes: self.limits.max_output_bytes,
            run_invocation_limit: self.limits.max_invocations_per_run,
        };
        intent.fingerprint = digest(&output::encode(&(&intent, &context.ancestry), 16 * 1024)?);
        output::reject_identity_secrets(&intent, &self.secrets)?;
        Ok(intent)
    }

    async fn execute(
        &self,
        context: &HookContext,
        hook: &EnabledHook,
        intent: &HookIntent,
        payload: Value,
        cancel: &dyn Cancellation,
    ) -> HookRecord {
        let mut ancestry = context.ancestry.clone();
        ancestry.push(HookAncestor {
            hook_key: hook.key(),
            invocation_id: intent.invocation_id.clone(),
        });
        let request = HookExecutionRequest {
            intent: intent.clone(),
            hook: hook.clone(),
            payload,
            next_depth: context.depth + 1,
            ancestry,
        };
        // begin must cooperate with cancellation and report ownership. Dropping
        // an approval/execution future cannot prove that a process never began.
        let begun = self.executor.begin(&request, cancel).await;
        let mut handle = match begun {
            Ok(HookBegin::Started(handle)) => handle,
            Ok(HookBegin::NotStarted { denied }) => {
                return failed_record(
                    intent,
                    if cancel.is_cancelled() {
                        HookStatus::Cancelled
                    } else if denied {
                        HookStatus::Denied
                    } else {
                        HookStatus::Failed
                    },
                    HookExecutionState::NotStarted,
                    CleanupState::Cleaned,
                    "hook_not_started",
                    "Host did not start the hook operation",
                );
            }
            Ok(HookBegin::Unknown) | Err(_) => {
                return failed_record(
                    intent,
                    HookStatus::Unknown,
                    HookExecutionState::Unknown,
                    CleanupState::Unknown,
                    "hook_unknown",
                    "Hook startup outcome requires independent reconciliation",
                );
            }
        };
        let deadline = tokio::time::sleep(Duration::from_millis(u64::from(intent.timeout_ms)));
        tokio::pin!(deadline);
        let (result, reason) = tokio::select! {
            biased;
            _ = cancel.cancelled() => (None, StopReason::Cancelled),
            _ = &mut deadline => (None, StopReason::TimedOut),
            result = handle.wait() => {
                let reason = if result.is_ok() { StopReason::Completed } else { StopReason::Failed };
                (Some(result), reason)
            }
        };
        let cleanup =
            match tokio::time::timeout(self.limits.cleanup_timeout, handle.stop(reason)).await {
                Ok(Ok(cleanup)) => cleanup,
                Ok(Err(_)) | Err(_) => CleanupState::Unknown,
            };
        if cleanup == CleanupState::Unknown {
            return failed_record(
                intent,
                HookStatus::Unknown,
                HookExecutionState::Started,
                cleanup,
                "hook_cleanup",
                "Hook cleanup remains unconfirmed",
            );
        }
        match reason {
            StopReason::Cancelled => failed_record(
                intent,
                HookStatus::Cancelled,
                HookExecutionState::Started,
                cleanup,
                "hook_cancelled",
                "Hook was cancelled and cleaned up",
            ),
            StopReason::TimedOut => failed_record(
                intent,
                HookStatus::TimedOut,
                HookExecutionState::Started,
                cleanup,
                "hook_timeout",
                "Hook execution exceeded its deadline and was cleaned up",
            ),
            _ => {
                let parsed = result
                    .and_then(Result::ok)
                    .ok_or_else(|| error("hook_execution", "Hook process execution failed"))
                    .and_then(|result| {
                        output::process_output(result, hook.kind, self.limits.max_output_bytes)
                    });
                match parsed {
                    Ok((decision, annotations)) => HookRecord {
                        intent: intent.clone(),
                        status: if decision == Some(HookDecision::Deny) {
                            HookStatus::Denied
                        } else {
                            HookStatus::Succeeded
                        },
                        execution: HookExecutionState::Started,
                        cleanup,
                        decision,
                        annotations,
                        error: None,
                    },
                    Err(failure) => failed_record(
                        intent,
                        HookStatus::Failed,
                        HookExecutionState::Started,
                        cleanup,
                        &failure.0.code,
                        &failure.0.message,
                    ),
                }
            }
        }
    }

    async fn note(
        &self,
        intent: &HookIntent,
        code: &str,
        message: &str,
        outcome: &mut HookBatchOutcome,
    ) -> Result<(), BoundaryError> {
        let note = output::diagnostic(
            HookDiagnostic {
                invocation_id: intent.invocation_id.clone(),
                event_id: intent.event_id.clone(),
                code: code.into(),
                message: message.into(),
            },
            &self.secrets,
        )?;
        self.journal
            .diagnostic(&note)
            .await
            .map_err(|_| error("hook_journal", "Hook diagnostic could not be saved"))?;
        outcome.diagnostics.push(note);
        Ok(())
    }
}

fn is_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn failed_record(
    intent: &HookIntent,
    status: HookStatus,
    execution: HookExecutionState,
    cleanup: CleanupState,
    code: &str,
    message: &str,
) -> HookRecord {
    HookRecord {
        intent: intent.clone(),
        status,
        execution,
        cleanup,
        decision: None,
        annotations: Vec::new(),
        error: Some(ErrorInfo {
            code: code.into(),
            message: message.into(),
        }),
    }
}
fn valid_record(record: &HookRecord) -> bool {
    match record.status {
        HookStatus::Succeeded => {
            record.execution == HookExecutionState::Started
                && record.cleanup == CleanupState::Cleaned
                && record.error.is_none()
                && match record.intent.kind {
                    HookKind::Observer => record.decision.is_none(),
                    HookKind::Check => matches!(
                        record.decision,
                        Some(HookDecision::Pass | HookDecision::Abstain)
                    ),
                }
        }
        HookStatus::Denied => {
            record.cleanup == CleanupState::Cleaned
                && ((record.execution == HookExecutionState::NotStarted
                    && record.decision.is_none())
                    || (record.execution == HookExecutionState::Started
                        && record.intent.kind == HookKind::Check
                        && record.decision == Some(HookDecision::Deny)))
        }
        HookStatus::Unknown => record.cleanup == CleanupState::Unknown,
        HookStatus::Failed | HookStatus::Cancelled | HookStatus::TimedOut => {
            record.cleanup == CleanupState::Cleaned
                && record.execution != HookExecutionState::Unknown
                && record.decision.is_none()
        }
    }
}
fn unsaved(record: &HookRecord) -> BoundaryError {
    let result = serde_json::to_value(record).ok().map(|value| {
        Box::new(crate::contract::ToolResult {
            ok: false,
            data: Some(serde_json::json!({"hook_record": value})),
            error: Some(ErrorInfo {
                code: "hook_journal".into(),
                message: "Hook result could not be saved".into(),
            }),
            truncated: false,
        })
    });
    BoundaryError(
        ErrorInfo {
            code: "hook_journal".into(),
            message: "Hook result could not be durably saved; its intent must not be replayed"
                .into(),
        },
        None,
        result,
    )
}
