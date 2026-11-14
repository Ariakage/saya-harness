use base64::Engine as _;
use saya_core::{
    boundary::Cancellation,
    browser::{
        network::BrowserNetworkSettings,
        vm::{
            BrowserNetworkFactory, BrowserVmSettings, SshVmCommandRunner,
            VmBrowserEnvironmentFactory, VmChromiumSandboxInspector, VmCommandRunner,
            VmWorkerIdentityVerifier, VmWorkerLaunchPlanner, VmWorkerLauncher,
        },
        worker::{
            BrowserEnvironmentFactory, ChromiumSandboxInspector, WorkerConnection,
            WorkerIdentityVerifier, WorkerLaunchPlanner, WorkerLauncher,
        },
    },
    mcp::McpContent,
    tools::exec::ExecConfig,
};
use std::{io::Cursor, path::PathBuf, sync::Arc, time::Duration};

struct NeverCancel;
impl Cancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("missing explicit test input {name}"))
}

fn settings() -> BrowserVmSettings {
    BrowserVmSettings {
        enabled: true,
        colima: Some(PathBuf::from(required("SAYA_TEST_VM_COLIMA"))),
        ssh: Some(PathBuf::from(required("SAYA_TEST_VM_SSH"))),
        profile: Some(required("SAYA_TEST_VM_PROFILE")),
        host: Some(required("SAYA_TEST_VM_HOST")),
        user: Some(required("SAYA_TEST_VM_USER")),
        port: Some(required("SAYA_TEST_VM_PORT").parse().unwrap()),
        identity_file: Some(PathBuf::from(required("SAYA_TEST_VM_IDENTITY_FILE"))),
        known_hosts: Some(PathBuf::from(required("SAYA_TEST_VM_KNOWN_HOSTS"))),
        node: Some(required("SAYA_TEST_VM_NODE")),
        worker_root: Some(required("SAYA_TEST_VM_WORKER_ROOT")),
        chromium: Some(required("SAYA_TEST_VM_CHROMIUM")),
        identity_manifest: Some(required("SAYA_TEST_VM_MANIFEST")),
        identity_sha256: Some(required("SAYA_TEST_VM_MANIFEST_SHA256")),
        remote_temp_root: Some(required("SAYA_TEST_VM_TEMP_ROOT")),
        request_timeout_ms: 15_000,
        ..Default::default()
    }
}

const PAUSED_MASK_PROBE: &str = r#"async (page) => {
  await page.setViewportSize({width:320,height:176});
  await page.setContent(`<!doctype html><body style="margin:0;background:white">
    <input id="otp" autocomplete="one-time-code" value="SYNTHETIC_OTP" style="position:absolute;left:16px;top:16px;width:128px;height:48px;border:0;padding:0;background:red;color:red">
    <input id="password" type="password" value="SYNTHETIC_PASSWORD" style="position:absolute;left:16px;top:80px;width:128px;height:48px;border:0;padding:0;background:red;color:red">
    <div id="closed" style="position:absolute;left:176px;top:16px"></div>
    <div style="position:absolute;left:176px;top:128px;width:128px;height:32px;background:rgb(0,255,0)"></div>
    <script>
      const root=document.querySelector('#closed').attachShadow({mode:'closed'});
      root.innerHTML='<input value="SYNTHETIC_CLOSED" style="width:128px;height:48px;border:0;padding:0;background:red;color:red">';
      let ticks=0;setInterval(()=>document.body.dataset.ticks=String(++ticks),10);
    </script>
  </body>`);
  await page.waitForTimeout(100);
  const client=await page.context().newCDPSession(page);
  const version=await client.send('Browser.getVersion');
  const changed=[];
  let paused=false;
  let evaluation;
  let image;
  let before;
  let during;
  let restored=true;
  try {
    await client.send('DOM.enable');
    await client.send('CSS.enable');
    await client.send('Debugger.enable');
    const stopped=new Promise(resolve=>client.once('Debugger.paused',resolve));
    evaluation=client.send('Runtime.evaluate',{expression:'debugger;'});
    evaluation.catch(()=>{});
    await Promise.race([stopped,new Promise((_,reject)=>setTimeout(()=>reject(new Error('pause not witnessed')),2000))]);
    paused=true;
    const tree=await client.send('DOM.getDocument',{depth:-1,pierce:true});
    const nodes=[];
    function walk(node){
      nodes.push(node);
      for(const child of node.children||[])walk(child);
      for(const child of node.shadowRoots||[])walk(child);
      if(node.contentDocument)walk(node.contentDocument);
    }
    walk(tree.root);
    const body=nodes.find(node=>node.localName==='body');
    const tick=async()=>{
      const {attributes}=await client.send('DOM.getAttributes',{nodeId:body.nodeId});
      return Number(attributes[attributes.indexOf('data-ticks')+1]);
    };
    before=await tick();
    const fields=nodes.filter(node=>node.localName==='input');
    if(fields.length!==3)throw new Error('closed-shadow input was not traversed');
    for(const node of fields){
      const attributes=node.attributes||[];
      const index=attributes.indexOf('style');
      const old=index<0?null:attributes[index+1];
      changed.push({nodeId:node.nodeId,old});
      await client.send('DOM.setAttributeValue',{nodeId:node.nodeId,name:'style',value:'visibility:hidden !important;opacity:0 !important;animation:none !important;transition:none !important'});
      const style=await client.send('CSS.getComputedStyleForNode',{nodeId:node.nodeId});
      if(!style.computedStyle.some(item=>item.name==='opacity'&&item.value==='0'))throw new Error('mask not computed');
    }
    image=await client.send('Page.captureScreenshot',{format:'png',fromSurface:true,captureBeyondViewport:false});
    await new Promise(resolve=>setTimeout(resolve,100));
    during=await tick();
  } finally {
    for(const item of changed.reverse()){
      if(item.old===null)await client.send('DOM.removeAttribute',{nodeId:item.nodeId,name:'style'});
      else await client.send('DOM.setAttributeValue',{nodeId:item.nodeId,name:'style',value:item.old});
      const {attributes}=await client.send('DOM.getAttributes',{nodeId:item.nodeId});
      const index=attributes.indexOf('style');
      restored=restored&&((index<0?null:attributes[index+1])===item.old);
    }
    if(paused)await client.send('Debugger.resume');
    if(evaluation)await evaluation;
    await client.detach();
  }
  await page.waitForTimeout(50);
  const after=Number(await page.locator('body').getAttribute('data-ticks'));
  return {png:image.data,fields:changed.length,before,during,after,restored,version:{product:version.product,revision:version.revision}};
}"#;

#[tokio::test]
#[ignore = "requires the explicit dedicated VM; characterizes paused-renderer pre-capture masking"]
async fn paused_renderer_paints_masks_inside_closed_shadow_and_restores_state() {
    assert_eq!(
        std::env::consts::OS,
        "macos",
        "requires the selected macOS VM host"
    );
    let image = std::fs::canonicalize(required("SAYA_TEST_EXECUTABLE")).unwrap();
    saya_core::execution::pin_launcher_image_from(&image).unwrap();
    let settings = settings();
    let exec = ExecConfig::default();
    let runner: Arc<dyn VmCommandRunner> = Arc::new(
        SshVmCommandRunner::new(settings.clone())
            .unwrap()
            .with_exec_config(exec.clone()),
    );
    let network = Arc::new(
        BrowserNetworkFactory::new(
            BrowserNetworkSettings {
                enabled: true,
                allowed_origins: vec!["https://example.com".into()],
                ..Default::default()
            },
            "masking-primitives",
        )
        .unwrap(),
    );
    let verifier = VmWorkerIdentityVerifier::new(settings.clone(), runner.clone());
    let identity = verifier.verify(&NeverCancel).await.unwrap();
    let factory = VmBrowserEnvironmentFactory::new(settings.clone()).unwrap();
    let lease = factory.prepare().unwrap();
    let planner = VmWorkerLaunchPlanner::new(settings.clone());
    let launcher = VmWorkerLauncher::new(settings.clone(), runner.clone(), exec, network);
    let inspector = VmChromiumSandboxInspector::new(settings, runner);
    let mut connection: Option<Arc<dyn WorkerConnection>> = None;
    let outcome = tokio::time::timeout(Duration::from_secs(40), async {
        let mut request = planner
            .build(&identity, lease.directories())
            .map_err(|error| format!("{error:?}"))?;
        request.execution = lease.execution();
        let live = launcher
            .launch(request, &NeverCancel)
            .await
            .map_err(|error| format!("{error:?}"))?;
        connection = Some(live.clone());
        let catalog = live
            .list_tools(&NeverCancel)
            .await
            .map_err(|error| format!("{error:?}"))?;
        live.call_tool(
            "browser_navigate",
            serde_json::json!({"url":"about:blank"}),
            catalog.generation,
            &NeverCancel,
        )
        .await
        .map_err(|error| format!("{error:?}"))?;
        let evidence = inspector
            .inspect(
                live.process_id().ok_or("worker PID missing")?,
                &live.stderr_tail(),
                &NeverCancel,
            )
            .await
            .map_err(|error| format!("{error:?}"))?;
        if !evidence.no_new_privs || evidence.seccomp_mode != 2 || !evidence.sandbox_arguments_valid
        {
            return Err("Chromium sandbox was not witnessed".to_owned());
        }
        let catalog = live
            .list_tools(&NeverCancel)
            .await
            .map_err(|error| format!("{error:?}"))?;
        let response = live
            .call_tool(
                "browser_run_code_unsafe",
                serde_json::json!({"code":PAUSED_MASK_PROBE}),
                catalog.generation,
                &NeverCancel,
            )
            .await
            .map_err(|error| format!("{error:?}"))?;
        if response.is_error || response.truncated {
            return Err(format!("bounded masking probe failed: {response:?}"));
        }
        let text = response
            .content
            .iter()
            .find_map(|item| match item {
                McpContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .ok_or("probe result missing")?;
        let result = text
            .split("### Result\n")
            .nth(1)
            .ok_or_else(|| text.chars().take(4096).collect::<String>())?;
        serde_json::from_str::<serde_json::Value>(
            result.split("\n### ").next().unwrap_or(result).trim(),
        )
        .map_err(|error| format!("probe JSON: {error}"))
    })
    .await;
    if let Some(connection) = connection {
        connection.shutdown().await.expect("owned worker cleanup");
    }
    launcher
        .retry_pending_cleanup()
        .await
        .expect("pending VM cleanup");
    lease.release().await.expect("host channel cleanup");
    let result = outcome
        .expect("bounded primitive probe")
        .expect("real primitive probe");
    assert_eq!(result["fields"], 3);
    assert!(
        result["version"]["product"]
            .as_str()
            .unwrap()
            .ends_with("/158.0.8127.18")
    );
    assert!(!result["version"]["revision"].as_str().unwrap().is_empty());
    eprintln!(
        "chromium_product={} chromium_revision={}",
        result["version"]["product"], result["version"]["revision"]
    );
    assert_eq!(result["restored"], true);
    assert!(result["before"].as_u64().unwrap() > 0);
    assert_eq!(result["before"], result["during"]);
    assert!(result["after"].as_u64().unwrap() > result["during"].as_u64().unwrap());
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(result["png"].as_str().unwrap())
        .unwrap();
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!((info.width, info.height), (320, 176));
    let channels = info.color_type.samples();
    assert!(channels >= 3);
    let pixel = |x: usize, y: usize| {
        let offset = (y * info.width as usize + x) * channels;
        &pixels[offset..offset + 3]
    };
    for (x, y) in [(32, 32), (192, 32), (32, 96)] {
        assert_eq!(
            pixel(x, y),
            [255, 255, 255],
            "sensitive field still painted at {x},{y}"
        );
    }
    assert_eq!(
        pixel(192, 144),
        [0, 255, 0],
        "non-sensitive source was lost"
    );
}
