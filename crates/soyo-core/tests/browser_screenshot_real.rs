use base64::Engine as _;
use soyo_core::{
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
        colima: Some(PathBuf::from(required("SOYO_TEST_VM_COLIMA"))),
        ssh: Some(PathBuf::from(required("SOYO_TEST_VM_SSH"))),
        profile: Some(required("SOYO_TEST_VM_PROFILE")),
        host: Some(required("SOYO_TEST_VM_HOST")),
        user: Some(required("SOYO_TEST_VM_USER")),
        port: Some(required("SOYO_TEST_VM_PORT").parse().unwrap()),
        identity_file: Some(PathBuf::from(required("SOYO_TEST_VM_IDENTITY_FILE"))),
        known_hosts: Some(PathBuf::from(required("SOYO_TEST_VM_KNOWN_HOSTS"))),
        node: Some(required("SOYO_TEST_VM_NODE")),
        worker_root: Some(required("SOYO_TEST_VM_WORKER_ROOT")),
        chromium: Some(required("SOYO_TEST_VM_CHROMIUM")),
        identity_manifest: Some(required("SOYO_TEST_VM_MANIFEST")),
        identity_sha256: Some(required("SOYO_TEST_VM_MANIFEST_SHA256")),
        remote_temp_root: Some(required("SOYO_TEST_VM_TEMP_ROOT")),
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

async fn run_vm_probe(code: &str, scope: &str) -> serde_json::Value {
    let image = std::fs::canonicalize(required("SOYO_TEST_EXECUTABLE")).unwrap();
    soyo_core::execution::pin_launcher_image_from(&image).unwrap();
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
            scope,
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
                serde_json::json!({"code":code}),
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
    outcome.expect("bounded VM probe").expect("real VM probe")
}

#[tokio::test]
#[ignore = "requires the explicit dedicated VM; characterizes paused-renderer pre-capture masking"]
async fn paused_renderer_paints_masks_inside_closed_shadow_and_restores_state() {
    let result = run_vm_probe(PAUSED_MASK_PROBE, "masking-primitives").await;
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
    assert_pre_capture_pixels(result["png"].as_str().unwrap());
}

const NATIVE_MASK_PROBE: &str = r#"async (page) => {
  await page.setViewportSize({width:320,height:176});
  await page.setContent(`<!doctype html><body style="margin:0;background:white">
    <input id="otp" autocomplete="one-time-code" value="SYNTHETIC_OTP" style="position:absolute;left:16px;top:16px;width:128px;height:48px;border:0;padding:0;background:red;color:red">
    <input id="password" type="password" value="SYNTHETIC_PASSWORD" style="position:absolute;left:16px;top:80px;width:128px;height:48px;border:0;padding:0;background:red;color:red">
    <div id="closed" style="position:absolute;left:176px;top:16px"></div>
    <div style="position:absolute;left:176px;top:128px;width:128px;height:32px;background:rgb(0,255,0)"></div>
  </body>`);
  await page.evaluate(() => {
    const root=document.querySelector('#closed').attachShadow({mode:'closed'});
    root.innerHTML='<input value="SYNTHETIC_CLOSED" style="width:128px;height:48px;border:0;padding:0;background:red;color:red">';
    window.__captureFields=[document.querySelector('#otp'),document.querySelector('#password'),root.querySelector('input')];
  });
  const state=()=>page.evaluate(()=>window.__captureFields.map(field=>[field.value,field.getAttribute('style')]));
  const before=await state();
  const client=await page.context().newCDPSession(page);
  try {
    const version=await client.send('Browser.getVersion');
    if(version.revision!=='@6449d9f9a6a906edff724b7b220199b3fde4615c')throw new Error('unexpected Chromium source revision');
    const binding=await client.send('HeadlessExperimental.getSoyoCaptureContext');
    if(binding.protocolVersion!==1||typeof binding.context!=='string'||binding.context.length===0)throw new Error('invalid native capture binding');
    const image=await client.send('HeadlessExperimental.captureSoyoScreenshot',{context:binding.context});
    if(image.context!==binding.context)throw new Error('native capture changed document binding');
    return {png:image.data,width:image.width,height:image.height,masked:image.masked,unchanged:JSON.stringify(before)===JSON.stringify(await state()),version};
  } finally {
    await client.detach();
  }
}"#;

#[tokio::test]
#[ignore = "requires the explicit dedicated VM and the native pre-capture engine"]
async fn native_capture_masks_fields_without_mutating_the_page() {
    let result = run_vm_probe(NATIVE_MASK_PROBE, "native-capture").await;
    assert_eq!(result["width"], 320);
    assert_eq!(result["height"], 176);
    assert_eq!(result["masked"], true);
    assert_eq!(result["unchanged"], true);
    assert_eq!(
        result["version"]["revision"],
        "@6449d9f9a6a906edff724b7b220199b3fde4615c"
    );
    assert_pre_capture_pixels(result["png"].as_str().unwrap());
}

const STALE_NATIVE_CONTEXT_PROBE: &str = r#"async (page) => {
  await page.setViewportSize({width:64,height:64});
  await page.setContent('<body style="margin:0;background:rgb(0,255,0)">');
  const client=await page.context().newCDPSession(page);
  try {
    const binding=await client.send('HeadlessExperimental.getSoyoCaptureContext');
    await page.evaluate(()=>history.pushState({},'', '#capture-changed'));
    try {
      await client.send('HeadlessExperimental.captureSoyoScreenshot',{context:binding.context});
      throw new Error('stale same-document context was accepted');
    } catch(error) {
      if(!String(error.message).includes('soyo_capture_stale_context'))throw error;
    }
    const fresh=await client.send('HeadlessExperimental.getSoyoCaptureContext');
    const image=await client.send('HeadlessExperimental.captureSoyoScreenshot',{context:fresh.context});
    return {changed:fresh.context!==binding.context,masked:image.masked,contextMatches:image.context===fresh.context};
  } finally {
    await client.detach();
  }
}"#;

#[tokio::test]
#[ignore = "requires the explicit dedicated VM and the native pre-capture engine"]
async fn native_capture_rejects_same_document_stale_context() {
    let result = run_vm_probe(STALE_NATIVE_CONTEXT_PROBE, "native-stale-context").await;
    assert_eq!(result["changed"], true);
    assert_eq!(result["masked"], true);
    assert_eq!(result["contextMatches"], true);
}

const NATIVE_CANVAS_PROBE: &str = r#"async (page) => {
  await page.setViewportSize({width:64,height:64});
  await page.setContent('<body style="margin:0"><canvas width="64" height="64"></canvas>');
  await page.evaluate(()=>{
    const context=document.querySelector('canvas').getContext('2d');
    context.fillStyle='red';context.fillRect(0,0,64,64);
  });
  const client=await page.context().newCDPSession(page);
  try {
    const binding=await client.send('HeadlessExperimental.getSoyoCaptureContext');
    const original=await client.send('HeadlessExperimental.captureSoyoScreenshot',{context:binding.context});
    await page.setContent('<body style="margin:0;background:rgb(0,255,0)">');
    const fresh=await client.send('HeadlessExperimental.getSoyoCaptureContext');
    const image=await client.send('HeadlessExperimental.captureSoyoScreenshot',{context:fresh.context});
    return {original:original.data,png:image.data,masked:original.masked&&image.masked,contextMatches:original.context===binding.context&&image.context===fresh.context};
  } finally {
    await client.detach();
  }
}"#;

#[tokio::test]
#[ignore = "requires the explicit dedicated VM and the native pre-capture engine"]
async fn native_capture_preserves_canvas_pixels_without_reusing_old_paint() {
    let result = run_vm_probe(NATIVE_CANVAS_PROBE, "native-canvas-paint").await;
    assert_eq!(result["masked"], true);
    assert_eq!(result["contextMatches"], true);
    for (name, expected) in [("original", [255, 0, 0]), ("png", [0, 255, 0])] {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(result[name].as_str().unwrap())
            .unwrap();
        let mut decoder = png::Decoder::new(Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((info.width, info.height), (64, 64));
        let channels = info.color_type.samples();
        assert!(channels >= 3);
        let offset = (16 * 64 + 16) * channels;
        assert_eq!(&pixels[offset..offset + 3], expected);
    }
}

const NATIVE_INLINE_PROBE: &str = r#"async (page) => {
  await page.setViewportSize({width:256,height:128});
  await page.setContent('<body style="margin:0;background:white"><p style="margin:0;line-height:48px"><span id="sensitive" contenteditable style="background:rgb(255,0,0);color:rgb(255,0,0);font:24px monospace">SYNTHETIC</span></p><div style="position:absolute;left:16px;top:96px;width:32px;height:16px;background:rgb(0,255,0)"></div>');
  const region=await page.evaluate(()=>{
    const box=document.querySelector('#sensitive').getBoundingClientRect();
    return {left:Math.ceil(box.left)+1,top:Math.ceil(box.top)+1,right:Math.floor(box.right)-1,bottom:Math.floor(box.bottom)-1};
  });
  const before=await page.content();
  const client=await page.context().newCDPSession(page);
  try {
    const binding=await client.send('HeadlessExperimental.getSoyoCaptureContext');
    const image=await client.send('HeadlessExperimental.captureSoyoScreenshot',{context:binding.context});
    return {png:image.data,region,masked:image.masked,unchanged:before===await page.content()};
  } finally {
    await client.detach();
  }
}"#;

#[tokio::test]
#[ignore = "requires the explicit dedicated VM and the native pre-capture engine"]
async fn native_capture_masks_inline_editable_text() {
    let result = run_vm_probe(NATIVE_INLINE_PROBE, "native-inline-editable").await;
    assert_eq!(result["masked"], true);
    assert_eq!(result["unchanged"], true);
    let (info, pixels) = decode_native_png(result["png"].as_str().unwrap());
    assert_eq!((info.width, info.height), (256, 128));
    let channels = info.color_type.samples();
    assert!(channels >= 3);
    let left = result["region"]["left"].as_u64().unwrap() as usize;
    let top = result["region"]["top"].as_u64().unwrap() as usize;
    let right = result["region"]["right"].as_u64().unwrap() as usize;
    let bottom = result["region"]["bottom"].as_u64().unwrap() as usize;
    assert!(left < right && right <= 256 && top < bottom && bottom <= 128);
    for y in top..bottom {
        for x in left..right {
            let sensitive = (y * 256 + x) * channels;
            assert_eq!(
                &pixels[sensitive..sensitive + 3],
                [255, 255, 255],
                "editable text still painted at {x},{y}"
            );
        }
    }
    let control = (100 * 256 + 24) * channels;
    assert_eq!(&pixels[control..control + 3], [0, 255, 0]);
}

const NATIVE_FLOAT_IMAGE_PROBE: &str = r#"async (page) => {
  await page.setViewportSize({width:256,height:128});
  await page.setContent(`<!doctype html><body style="margin:0;background:white">
    <div contenteditable style="display:contents"><img id="private" src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z/D/PwAG/gL+DHWJ3gAAAABJRU5ErkJggg==" style="float:left;width:128px;height:48px"></div>
    <div style="position:absolute;left:176px;top:96px;width:32px;height:16px;background:rgb(0,255,0)"></div>
  </body>`);
  await page.locator('#private').evaluate(async image=>{
    await image.decode();
    if(!image.isContentEditable||getComputedStyle(image.parentElement).display!=='contents')throw new Error('sensitive floating image fixture was not established');
  });
  const region=await page.locator('#private').evaluate(image=>{
    const box=image.getBoundingClientRect();
    return {left:Math.ceil(box.left)+4,top:Math.ceil(box.top)+4,right:Math.floor(box.right)-4,bottom:Math.floor(box.bottom)-4};
  });
  const before=await page.content();
  const client=await page.context().newCDPSession(page);
  try {
    const reference=await client.send('Page.captureScreenshot',{format:'png',fromSurface:true,captureBeyondViewport:false});
    let binding,image,failure=null;
    try {
      binding=await client.send('HeadlessExperimental.getSoyoCaptureContext');
      image=await client.send('HeadlessExperimental.captureSoyoScreenshot',{context:binding.context});
    } catch(error) {
      failure=String(error.message);
    }
    return {reference:reference.data,png:image?.data,region,failure,masked:image?.masked,contextMatches:!!image&&image.context===binding.context,unchanged:before===await page.content()};
  } finally {
    await client.detach();
  }
}"#;

#[tokio::test]
#[ignore = "requires the explicit dedicated VM and the native pre-capture engine"]
async fn native_capture_masks_floating_images_under_display_contents_editables() {
    let result = run_vm_probe(NATIVE_FLOAT_IMAGE_PROBE, "native-sensitive-float-image").await;
    assert_capture_fixture_pixels(
        result["reference"].as_str().unwrap(),
        &result["region"],
        [255, 0, 255],
    );
    assert!(
        result["failure"].is_null(),
        "native capture: {}",
        result["failure"]
    );
    assert_eq!(result["masked"], true);
    assert_eq!(result["contextMatches"], true);
    assert_eq!(result["unchanged"], true);
    assert_capture_fixture_pixels(
        result["png"].as_str().unwrap(),
        &result["region"],
        [255, 255, 255],
    );
}

const NATIVE_FIRST_LINE_IMAGE_PROBE: &str = r#"async (page) => {
  await page.setViewportSize({width:256,height:128});
  const url='https://example.com/saya-native-first-line.png';
  const fulfill=route=>route.fulfill({status:200,contentType:'image/png',body:Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z/D/PwAG/gL+DHWJ3gAAAABJRU5ErkJggg==','base64')});
  await page.route(url,fulfill);
  const client=await page.context().newCDPSession(page);
  try {
    await page.setContent(`<!doctype html><style>p{margin:0;font:24px monospace;line-height:48px;color:transparent}p::first-line{background-image:url('${url}')}</style><body style="margin:0;background:white"><p id="line">FIRST_LINE</p><div style="position:absolute;left:176px;top:96px;width:32px;height:16px;background:rgb(0,255,0)"></div></body>`);
    const originBlocked=await page.evaluate(async url=>{
      const image=new Image();image.src=url;await image.decode();
      const canvas=document.createElement('canvas');canvas.width=1;canvas.height=1;
      const context=canvas.getContext('2d');context.drawImage(image,0,0);
      try {context.getImageData(0,0,1,1);return false;}
      catch(error) {if(error.name!=='SecurityError')throw error;return true;}
    },url);
    if(!originBlocked)throw new Error('first-line image fixture was not cross-origin readback-blocked');
    const region=await page.locator('#line').evaluate(line=>{
      if(getComputedStyle(line).backgroundImage!=='none'||getComputedStyle(line,'::first-line').backgroundImage==='none')throw new Error('first-line-only background fixture was not established');
      const range=document.createRange();range.selectNodeContents(line);
      const box=range.getBoundingClientRect();
      return {left:Math.ceil(box.left)+4,top:Math.ceil(box.top)+4,right:Math.floor(box.right)-4,bottom:Math.floor(box.bottom)-4};
    });
    const reference=await client.send('Page.captureScreenshot',{format:'png',fromSurface:true,captureBeyondViewport:false});
    let image,failure=null;
    try {
      const binding=await client.send('HeadlessExperimental.getSoyoCaptureContext');
      image=await client.send('HeadlessExperimental.captureSoyoScreenshot',{context:binding.context});
    } catch(error) {
      failure=String(error.message);
    }
    return {reference:reference.data,region,originBlocked,accepted:!!image,failure};
  } finally {
    await client.detach();
    await page.unroute(url,fulfill);
  }
}"#;

#[tokio::test]
#[ignore = "requires the explicit dedicated VM and the native pre-capture engine"]
async fn native_capture_rejects_unreadable_first_line_images_instead_of_certifying_gaps() {
    let result = run_vm_probe(NATIVE_FIRST_LINE_IMAGE_PROBE, "native-first-line-image").await;
    assert_eq!(result["originBlocked"], true);
    assert_capture_fixture_pixels(
        result["reference"].as_str().unwrap(),
        &result["region"],
        [255, 0, 255],
    );
    assert_eq!(result["accepted"], false);
    assert!(
        result["failure"]
            .as_str()
            .unwrap_or_default()
            .contains("soyo_capture_incomplete"),
        "native capture: {}",
        result["failure"]
    );
}

fn assert_capture_fixture_pixels(encoded: &str, region: &serde_json::Value, expected: [u8; 3]) {
    let (info, pixels) = decode_native_png(encoded);
    assert_eq!((info.width, info.height), (256, 128));
    let channels = info.color_type.samples();
    assert!(channels >= 3);
    let left = region["left"].as_u64().unwrap() as usize;
    let top = region["top"].as_u64().unwrap() as usize;
    let right = region["right"].as_u64().unwrap() as usize;
    let bottom = region["bottom"].as_u64().unwrap() as usize;
    assert!(left < right && right <= 256 && top < bottom && bottom <= 128);
    for y in top..bottom {
        for x in left..right {
            let offset = (y * 256 + x) * channels;
            assert_eq!(
                &pixels[offset..offset + 3],
                expected,
                "unexpected capture fixture pixel at {x},{y}"
            );
        }
    }
    let control = (100 * 256 + 184) * channels;
    assert_eq!(&pixels[control..control + 3], [0, 255, 0]);
}

const NATIVE_SCROLL_PROBE: &str = r#"async (page) => {
  await page.setViewportSize({width:64,height:64});
  await page.setContent('<html style="scrollbar-width:none"><body style="margin:0;height:128px"><div style="position:absolute;left:0;top:0;width:64px;height:64px;background:rgb(255,0,0)"></div><div style="position:absolute;left:0;top:64px;width:64px;height:64px;background:rgb(0,255,0)"></div></body></html>');
  const scroll=await page.evaluate(()=>{window.scrollTo(0,64);return window.scrollY;});
  if(scroll!==64)throw new Error('scroll fixture was not positioned');
  const client=await page.context().newCDPSession(page);
  try {
    const binding=await client.send('HeadlessExperimental.getSoyoCaptureContext');
    const image=await client.send('HeadlessExperimental.captureSoyoScreenshot',{context:binding.context});
    return {png:image.data,masked:image.masked,scroll};
  } finally {
    await client.detach();
  }
}"#;

#[tokio::test]
#[ignore = "requires the explicit dedicated VM and the native pre-capture engine"]
async fn native_capture_uses_the_current_scrolled_viewport() {
    let result = run_vm_probe(NATIVE_SCROLL_PROBE, "native-scrolled-viewport").await;
    assert_eq!(result["masked"], true);
    assert_eq!(result["scroll"], 64);
    let (info, pixels) = decode_native_png(result["png"].as_str().unwrap());
    assert_eq!((info.width, info.height), (64, 64));
    let channels = info.color_type.samples();
    assert!(channels >= 3);
    let offset = (16 * 64 + 16) * channels;
    assert_eq!(&pixels[offset..offset + 3], [0, 255, 0]);
}

const NATIVE_TRANSPARENT_PROBE: &str = r#"async (page) => {
  await page.setViewportSize({width:64,height:64});
  await page.setContent('<html style="background:transparent"><body style="margin:0;background:rgb(255,0,0)"></body></html>');
  const client=await page.context().newCDPSession(page);
  try {
    await client.send('Emulation.setDefaultBackgroundColorOverride',{color:{r:0,g:0,b:0,a:0}});
    const binding=await client.send('HeadlessExperimental.getSoyoCaptureContext');
    const original=await client.send('HeadlessExperimental.captureSoyoScreenshot',{context:binding.context});
    await page.evaluate(()=>document.body.style.background='transparent');
    const fresh=await client.send('HeadlessExperimental.getSoyoCaptureContext');
    const image=await client.send('HeadlessExperimental.captureSoyoScreenshot',{context:fresh.context});
    return {original:original.data,png:image.data,masked:original.masked&&image.masked};
  } finally {
    await client.send('Emulation.setDefaultBackgroundColorOverride',{});
    await client.detach();
  }
}"#;

#[tokio::test]
#[ignore = "requires the explicit dedicated VM and the native pre-capture engine"]
async fn native_capture_initializes_transparent_pixels() {
    let result = run_vm_probe(NATIVE_TRANSPARENT_PROBE, "native-transparent-pixels").await;
    assert_eq!(result["masked"], true);
    let (original, original_pixels) = decode_native_png(result["original"].as_str().unwrap());
    assert_eq!((original.width, original.height), (64, 64));
    let channels = original.color_type.samples();
    assert!(channels >= 3);
    let offset = (16 * 64 + 16) * channels;
    assert_eq!(&original_pixels[offset..offset + 3], [255, 0, 0]);
    let (info, pixels) = decode_native_png(result["png"].as_str().unwrap());
    assert_eq!((info.width, info.height), (64, 64));
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert!(pixels.iter().all(|byte| *byte == 0));
}

const NATIVE_INCOMPLETE_PROBE: &str = r#"async (page) => {
  await page.setViewportSize({width:64,height:64});
  await page.setContent('<body style="margin:0"><svg width="64" height="64"><rect width="64" height="64" fill="red"/></svg></body>');
  const client=await page.context().newCDPSession(page);
  try {
    const binding=await client.send('HeadlessExperimental.getSoyoCaptureContext');
    let failure;
    try {
      await client.send('HeadlessExperimental.captureSoyoScreenshot',{context:binding.context});
    } catch(error) {
      failure=String(error.message);
    }
    if(!failure||!failure.includes('soyo_capture_incomplete'))throw new Error('unsupported native paint was not classified as incomplete: '+failure);
    await page.setContent('<body style="margin:0;background:rgb(0,255,0)">');
    const fresh=await client.send('HeadlessExperimental.getSoyoCaptureContext');
    const image=await client.send('HeadlessExperimental.captureSoyoScreenshot',{context:fresh.context});
    return {png:image.data,masked:image.masked,contextMatches:image.context===fresh.context};
  } finally {
    await client.detach();
  }
}"#;

#[tokio::test]
#[ignore = "requires the explicit dedicated VM and the native pre-capture engine"]
async fn native_capture_rejects_incomplete_paint_without_fabricating_stale_context() {
    let result = run_vm_probe(NATIVE_INCOMPLETE_PROBE, "native-incomplete-paint").await;
    assert_eq!(result["masked"], true);
    assert_eq!(result["contextMatches"], true);
    let (info, pixels) = decode_native_png(result["png"].as_str().unwrap());
    assert_eq!((info.width, info.height), (64, 64));
    let channels = info.color_type.samples();
    assert!(channels >= 3);
    let offset = (16 * 64 + 16) * channels;
    assert_eq!(&pixels[offset..offset + 3], [0, 255, 0]);
}

const NATIVE_TIMEOUT_PROBE: &str = r#"async (page) => {
  await page.setViewportSize({width:64,height:64});
  await page.setContent('<body style="margin:0;background:rgb(0,255,0)">');
  const client=await page.context().newCDPSession(page);
  let paused=false;
  let evaluation;
  try {
    const binding=await client.send('HeadlessExperimental.getSoyoCaptureContext');
    await client.send('Debugger.enable');
    const stopped=new Promise(resolve=>client.once('Debugger.paused',resolve));
    evaluation=client.send('Runtime.evaluate',{expression:'debugger;'});
    evaluation.catch(()=>{});
    await Promise.race([stopped,new Promise((_,reject)=>setTimeout(()=>reject(new Error('pause not witnessed')),2000))]);
    paused=true;
    let failure;
    try {
      await client.send('HeadlessExperimental.captureSoyoScreenshot',{context:binding.context});
    } catch(error) {
      failure=String(error.message);
    }
    if(!failure||!failure.includes('soyo_capture_timeout'))throw new Error('native capture deadline was not classified as timeout: '+failure);
    return {timeout:true};
  } finally {
    if(paused)await client.send('Debugger.resume');
    if(evaluation)await evaluation;
    await client.detach();
  }
}"#;

#[tokio::test]
#[ignore = "requires the explicit dedicated VM and the native pre-capture engine"]
async fn native_capture_reports_a_native_deadline_without_fabricating_stale_context() {
    let result = run_vm_probe(NATIVE_TIMEOUT_PROBE, "native-capture-timeout").await;
    assert_eq!(result["timeout"], true);
}

fn decode_native_png(encoded: &str) -> (png::OutputInfo, Vec<u8>) {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    pixels.truncate(info.buffer_size());
    (info, pixels)
}

fn assert_pre_capture_pixels(encoded: &str) {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
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

#[tokio::test]
#[ignore]
async fn production_provider_captures_masked_png_and_hides_credentials() {
    use soyo_core::{
        boundary::ToolContext,
        browser::{
            BrowserCredentialPurpose, BrowserCredentialSettings, BrowserCredentialSlot,
            BrowserSettings, BrowserState, vm::assemble_vm_browser_provider,
        },
        contract::{PermissionMode, Run, RunId, SessionId, WorkMode},
        credentials::{
            CredentialScope, CredentialValue, FileCredentialStore, SecretRegistry, SecretValue,
        },
        output_store::{OutputLimits, OutputStore},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    let image = std::fs::canonicalize(required("SOYO_TEST_EXECUTABLE")).unwrap();
    soyo_core::execution::pin_launcher_image_from(&image).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let html = "<!doctype html><html><body style=\"margin:0;background:#ffffff\">\
            <input id=\"pw\" type=\"password\" aria-label=\"secret password\" \
            style=\"position:fixed;left:16px;top:16px;width:96px;height:32px;border:0;padding:0;background:#ff0000;color:#0000ff;font:16px monospace\">\
            <input id=\"otp\" autocomplete=\"one-time-code\" value=\"998877\" aria-label=\"otp code\" \
            style=\"position:fixed;left:16px;top:64px;width:96px;height:32px;border:0;padding:0;background:#ff0000;color:#0000ff;font:16px monospace\">\
            <div style=\"position:fixed;left:16px;top:128px;width:96px;height:32px;background:#00ff00\"></div>\
            </body></html>";
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    html.len(),
                    html
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });

    let root = tempfile::tempdir().unwrap();
    let registry = Arc::new(SecretRegistry::default());
    let credential_dir = root.path().join("credentials");
    let credential_store = FileCredentialStore::open(&credential_dir, registry.clone()).unwrap();
    let credential_ref = credential_store
        .create(
            CredentialScope::for_browser(
                "login",
                &soyo_core::network::NormalizedOrigin::parse(&origin).unwrap(),
            )
            .unwrap(),
            CredentialValue::Secret(SecretValue::new("alice-secret").unwrap()),
        )
        .await
        .unwrap();

    let network = BrowserNetworkSettings {
        enabled: true,
        allowed_origins: vec![origin.clone()],
        public_web: false,
        localhost_development_origin: Some(origin.clone()),
        ..Default::default()
    };
    let browser_settings = BrowserSettings {
        enabled: true,
        max_wait_ms: 30_000,
        credentials: BrowserCredentialSettings {
            enabled: true,
            store: Some(credential_dir),
            slots: vec![BrowserCredentialSlot {
                alias: "login".into(),
                credential_ref,
                origin: origin.clone(),
                purpose: BrowserCredentialPurpose::FormFill,
                max_uses: 1,
            }],
        },
        network: network.clone(),
        ..Default::default()
    };
    let outputs = Arc::new(
        OutputStore::open(
            root.path().join("artifacts"),
            OutputLimits::default(),
            registry.clone(),
        )
        .unwrap(),
    );
    let run = Run {
        id: RunId::new(),
        session_id: SessionId::new(),
        work_mode: WorkMode::Work,
        permissions: PermissionMode::Bypass,
    };
    let provider = Arc::new(
        assemble_vm_browser_provider(
            settings(),
            network,
            &run.id.0.to_string(),
            ExecConfig::default(),
        )
        .expect("VM provider assembly"),
    );
    let state = Arc::new(
        BrowserState::new_with_services(
            browser_settings,
            provider,
            run.clone(),
            Some(outputs.clone()),
            None,
            registry,
            soyo_core::config::MediaCapabilities::default(),
        )
        .unwrap(),
    );
    let context = ToolContext {
        run: run.clone(),
        workspace: root.path().to_path_buf(),
    };

    assert!(state.facts().screenshots);
    assert!(state.facts().screenshot_masking);
    let opened = state.open(&context, &NeverCancel).await.expect("open");
    let target_url = format!("{origin}/masked");
    state
        .navigate(&context, &opened.page, &target_url, &NeverCancel)
        .await
        .expect("navigate masked");
    let snap = state
        .snapshot(&context, &opened.page, &NeverCancel)
        .await
        .expect("snapshot masked");
    let password_field = snap
        .elements
        .iter()
        .find(|element| element.name == "secret password")
        .expect("secret password input in snapshot");
    state
        .fill_credential(
            &context,
            &opened.page,
            &snap.document,
            &password_field.element_ref,
            "login",
            &NeverCancel,
        )
        .await
        .expect("fill password credential");

    let (shot, sealed) = state
        .screenshot(&context, &opened.page, &NeverCancel)
        .await
        .expect("production masked screenshot");
    assert!(shot.masked);
    assert_eq!(shot.mime, "image/png");
    assert_eq!(shot.url.as_deref(), None);
    assert_eq!(shot.url_observation, "last_confirmed");
    let artifact_ref = shot.artifact_ref.as_deref().expect("artifact ref");
    assert!(sealed.is_some());

    // After a fresh snapshot confirms the URL, `last_confirmed` tracks `target_url`.
    state
        .snapshot(&context, &opened.page, &NeverCancel)
        .await
        .expect("snapshot after fill");
    let (shot_after_snap, sealed_after_snap) = state
        .screenshot(&context, &opened.page, &NeverCancel)
        .await
        .expect("production masked screenshot after snapshot");
    assert!(shot_after_snap.masked);
    assert_eq!(shot_after_snap.url.as_deref(), Some(target_url.as_str()));
    assert_eq!(shot_after_snap.url_observation, "last_confirmed");
    assert!(sealed_after_snap.is_some());

    let loaded = outputs
        .load_media(run.session_id, artifact_ref)
        .expect("load screenshot artifact");
    let raw_png = base64::engine::general_purpose::STANDARD
        .decode(&loaded.data)
        .expect("decode artifact png");
    assert!(
        !raw_png
            .windows(12)
            .any(|w| w == b"alice-secret" || w == b"998877\0\0\0\0\0\0"),
        "PNG bytes must not leak secret"
    );
    let (info, pixels) = decode_native_png(&loaded.data);
    let channels = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        other => panic!("unexpected PNG color type {other:?}"),
    };
    let pixel = |x: usize, y: usize| -> &[u8] {
        let offset = (y * info.width as usize + x) * channels;
        &pixels[offset..offset + 3]
    };
    for (x, y) in [(48, 32), (48, 80)] {
        assert_eq!(
            pixel(x, y),
            [255, 255, 255],
            "sensitive field leaked at {x},{y}"
        );
    }
    assert_eq!(
        pixel(48, 144),
        [0, 255, 0],
        "non-sensitive element was not captured"
    );

    state.shutdown(&NeverCancel).await.expect("shutdown");
    server.abort();
}
