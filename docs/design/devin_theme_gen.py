#!/usr/bin/env python3
"""Generate Devin-themed HTML mocks of the Soyo desktop app for html_to_figma.

Run from the repo root or any directory:

    python3 docs/design/devin_theme_gen.py [output_dir]

Each frame is written as a standalone HTML file sized 1520x980. Import the
files into the Figma "Theme" page with the html_to_figma MCP flow, then
reposition the imported nodes onto the existing frame grid.
"""
import base64
import os
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
ICON_B64 = base64.b64encode(
    open(REPO_ROOT / "desktop" / "public" / "app-icon.png", "rb").read()
).decode()
ICON = f"data:image/png;base64,{ICON_B64}"
OUT = sys.argv[1] if len(sys.argv) > 1 else "/tmp/soyo_devin_theme"
os.makedirs(OUT, exist_ok=True)

CSS = """
*{box-sizing:border-box;margin:0;padding:0;border-color:#2a2a2a}
html,body{height:100%}
body{background:#101010;color:#e8e8e8;font-family:-apple-system,BlinkMacSystemFont,"SF Pro Text","Segoe UI",Inter,sans-serif;font-size:13px;-webkit-font-smoothing:antialiased;overflow:hidden}
.app{display:flex;height:100vh;width:100vw}
.sidebar{width:232px;flex:none;background:#0d0d0d;border-right:1px solid #1f1f1f;display:flex;flex-direction:column;padding:8px}
.rail{width:56px;flex:none;background:#0d0d0d;border-right:1px solid #1f1f1f;display:flex;flex-direction:column;align-items:center;padding:10px 0;gap:2px}
.rail .rbtn{width:36px;height:36px;border-radius:9px;display:flex;align-items:center;justify-content:center;color:#8a8a8a}
.rail .rbtn.active{background:#1e1e1e;color:#e8e8e8}
.rail .rbtn:hover{background:#1a1a1a}
.nav{display:flex;flex-direction:column;gap:1px;margin-top:6px}
.nav a{display:flex;align-items:center;gap:9px;padding:6px 10px;border-radius:7px;color:#c9c9c9;font-size:13px}
.nav a:hover{background:#1a1a1a;color:#e8e8e8}
.nav a svg{width:15px;height:15px;color:#8a8a8a;flex:none}
.section-label{margin:16px 10px 4px;font-size:11px;color:#6b6b6b;display:flex;align-items:center;justify-content:space-between}
.recent{display:flex;flex-direction:column;gap:1px}
.recent a{padding:6px 10px;border-radius:7px;color:#a3a3a3;font-size:12.5px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.recent a:hover{background:#1a1a1a;color:#d9d9d9}
.recent a.sel{background:#1c1c1c;color:#e8e8e8}
.user-chip{margin-top:auto;display:flex;align-items:center;gap:8px;padding:8px 10px;border-radius:9px;color:#a3a3a3;font-size:12.5px}
.user-chip img{width:20px;height:20px;border-radius:50%;object-fit:cover}
.user-chip:hover{background:#1a1a1a}
.main{flex:1;display:flex;flex-direction:column;min-width:0;background:#141414}
.topbar{height:44px;flex:none;display:flex;align-items:center;justify-content:space-between;padding:0 14px;border-bottom:1px solid #1f1f1f;color:#a3a3a3;font-size:12.5px}
.content{flex:1;overflow:hidden;display:flex;flex-direction:column;position:relative}
.center-home{flex:1;display:flex;flex-direction:column;align-items:center;justify-content:center;padding-bottom:60px}
.greet{display:flex;align-items:center;gap:12px;font-family:Georgia,"New York",serif;font-size:30px;color:#e8e8e8;letter-spacing:.2px}
.greet img{width:30px;height:30px;border-radius:50%}
.composer{width:620px;background:#1d1d1d;border:1px solid #2b2b2b;border-radius:14px;padding:12px 14px;box-shadow:0 8px 30px rgba(0,0,0,.35)}
.composer .ta{color:#e8e8e8;font-size:13.5px;min-height:22px}
.composer .ta.ph{color:#6e6e6e}
.composer .row{display:flex;align-items:center;justify-content:space-between;margin-top:10px}
.chip{display:inline-flex;align-items:center;gap:6px;color:#a3a3a3;font-size:12px;padding:4px 8px;border-radius:8px}
.chip:hover{background:#262626}
.chip .dot{width:6px;height:6px;border-radius:3px}
.send{width:28px;height:28px;border-radius:50%;background:#d9a08e;color:#1c120e;display:flex;align-items:center;justify-content:center}
.send.off{background:#2e2e2e;color:#7a7a7a}
.ideas{margin-top:34px;display:flex;flex-direction:column;gap:2px;width:620px}
.ideas .lbl{font-size:11.5px;color:#6b6b6b;padding:4px 12px}
.ideas a{display:flex;align-items:center;gap:12px;padding:9px 12px;border-radius:9px;color:#bdbdbd;font-size:13px}
.ideas a:hover{background:#1a1a1a}
.ideas .ib{width:26px;height:26px;border-radius:7px;background:#1d1d1d;border:1px solid #2b2b2b;display:flex;align-items:center;justify-content:center;color:#8a8a8a;flex:none}
.thread{flex:1;overflow:hidden;padding:18px 26px 8px;display:flex;flex-direction:column;gap:14px}
.msg-user{align-self:flex-end;max-width:640px;background:#242424;border:1px solid #2e2e2e;border-radius:14px;padding:9px 14px;font-size:13.5px;color:#ececec}
.msg-sys{color:#6e6e6e;font-size:12px;padding:2px 14px}
.msg-sys a{color:#8aa8d8}
.card{background:#1a1a1a;border:1px solid #262626;border-radius:12px;padding:12px 14px}
.md-plain{padding:2px 0}
.toolcard{display:flex;align-items:center;gap:10px;font-size:12.5px;color:#9a9a9a;padding:7px 2px}
.thinking{display:flex;align-items:center;gap:8px;color:#8a8a8a;font-size:12.5px;padding:6px 2px}
.thinking .chev{color:#6e6e6e;font-size:13px}
.thinking-body{border-left:2px solid #2c2c2c;margin:2px 2px 4px 7px;padding:2px 0 2px 14px;color:#8a8a8a;font-size:12.5px;line-height:1.65}
.toolcard .tname{font-family:ui-monospace,Menlo,monospace;color:#e8e8e8}
.badge{display:inline-flex;align-items:center;gap:5px;font-size:12px;padding:3px 10px;border-radius:8px;font-weight:500;background:#1c1c1c;color:#a3a3a3;border:1px solid #2e2e2e}
.badge.green{color:#4ade80}
.badge.blue{color:#7aa2f7}
.badge.amber{color:#fac454}
.badge.plain{color:#a3a3a3}
.md h3{font-size:15px;margin:8px 0 6px;color:#ececec}
.md a{color:#8aa8d8;text-decoration:none}
.md p{color:#c9c9c9;line-height:1.65;font-size:13.5px;margin:6px 0}
.md ul{color:#c9c9c9;line-height:1.7;font-size:13.5px;padding-left:20px;margin:6px 0}
.md code{font-family:ui-monospace,Menlo,monospace;font-size:12px;background:#222;border:1px solid #2c2c2c;padding:1px 5px;border-radius:5px;color:#e8e8e8}
.codeblock{background:#161616;border:1px solid #262626;border-radius:10px;padding:10px 12px;font-family:ui-monospace,Menlo,monospace;font-size:12px;line-height:1.6;color:#c9d1d9;margin:8px 0}
.codeblock .k{color:#ff7b72}.codeblock .s{color:#a5d6ff}.codeblock .f{color:#d2a8ff}.codeblock .c{color:#6e6e6e}
.composer-bar{flex:none;padding:10px 26px 14px;display:flex;justify-content:center}
.composer-bar .inner{width:100%;max-width:840px}
.kbd-hint{color:#5e5e5e;font-size:11px;display:flex;gap:14px;justify-content:center;margin-top:8px}
.panel{width:308px;flex:none;border-left:1px solid #1f1f1f;background:#0f0f0f;overflow:hidden;padding:14px 14px;display:flex;flex-direction:column;gap:14px}
.panel h4{font-size:12px;color:#8a8a8a;font-weight:500;display:flex;align-items:center;justify-content:space-between}
.panel .prow{display:flex;justify-content:space-between;font-size:12px;color:#c9c9c9;padding:5px 0}
.panel .prow .k{color:#7a7a7a}
.ctxbar{display:flex;height:6px;border-radius:3px;overflow:hidden;margin:8px 0 10px}
.ctxbar div{height:100%}
.legend{display:flex;flex-direction:column;gap:5px;font-size:11.5px}
.legend .li{display:flex;align-items:center;justify-content:space-between;color:#a3a3a3}
.legend .li .l{display:flex;align-items:center;gap:7px}
.legend .sw{width:8px;height:8px;border-radius:2px}
.approval{background:#1c1a15;border:1px solid #4a3f2a;border-radius:12px;padding:14px}
.approval .ttl{display:flex;align-items:center;justify-content:space-between;color:#e8d9b0;font-size:13px;font-weight:600}
.approval .kv{display:grid;grid-template-columns:auto 1fr;gap:3px 12px;font-size:12px;margin:10px 0;background:#171512;border:1px solid #2e2820;border-radius:9px;padding:10px 12px}
.approval .kv .k{color:#8a8a8a}
.approval .kv .v{color:#d9d9d9;font-family:ui-monospace,Menlo,monospace;font-size:11.5px}
.approval .cmd{background:#121110;border:1px solid #2e2820;border-radius:8px;font-family:ui-monospace,Menlo,monospace;font-size:12px;color:#e8e8e8;padding:8px 10px;margin:8px 0}
.btnrow{display:flex;gap:8px;justify-content:flex-end;margin-top:10px}
.btn{padding:5px 14px;border-radius:8px;font-size:12px;font-weight:500;border:1px solid #2e2e2e;background:#1c1c1c;color:#d9d9d9}
.btn.deny{color:#f87171}
.btn.allow{color:#4ade80}
.btn.primary{background:#e8e8e8;color:#111}
.sessions-col{width:248px;flex:none;background:#0f0f0f;border-right:1px solid #1f1f1f;padding:10px;display:flex;flex-direction:column}
.sess-item{padding:8px 10px;border-radius:8px;color:#a3a3a3;font-size:12.5px;display:flex;flex-direction:column;gap:2px}
.sess-item:hover{background:#1a1a1a}
.sess-item .t{color:#d9d9d9;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.sess-item .meta{font-size:11px;color:#6e6e6e}
.sess-item.sel{background:#1c1c1c}
.overlay{position:absolute;inset:0;background:rgba(0,0,0,.55);display:flex;align-items:center;justify-content:center}
.dialog{width:640px;background:#181818;border:1px solid #2b2b2b;border-radius:14px;overflow:hidden;box-shadow:0 24px 60px rgba(0,0,0,.5);display:flex}
.dialog .dnav{width:180px;flex:none;border-right:1px solid #232323;padding:12px 8px;display:flex;flex-direction:column;gap:1px}
.dialog .dnav a{display:flex;align-items:center;gap:8px;padding:6px 9px;border-radius:7px;color:#a3a3a3;font-size:12.5px}
.dialog .dnav a.sel{background:#222;color:#e8e8e8}
.dialog .dbody{flex:1;padding:16px;overflow:hidden}
.dialog .dbody h3{font-size:14px;color:#e8e8e8;margin-bottom:12px}
.field{margin-bottom:16px}
.field .fl{font-size:12px;color:#a3a3a3;margin-bottom:5px}
.field .fv{background:#1e1e1e;border:1px solid #2b2b2b;border-radius:8px;padding:7px 10px;font-size:12.5px;color:#d9d9d9;display:flex;justify-content:space-between;align-items:center}
.seg{display:inline-flex;background:#1e1e1e;border:1px solid #2b2b2b;border-radius:8px;padding:2px;gap:2px}
.seg span{padding:4px 12px;border-radius:6px;font-size:12px;color:#a3a3a3}
.seg span.on{background:#2e2e2e;color:#e8e8e8}
.switch{width:34px;height:20px;border-radius:5px;background:#d9a08e;position:relative}
.switch::after{content:"";position:absolute;width:15px;height:15px;border-radius:4px;background:#fff;top:2.5px;right:2.5px}
.switch.off{background:#3a3a3a}
.switch.off::after{left:2.5px;right:auto}
.menu{position:absolute;left:10px;bottom:52px;width:190px;background:#1c1c1c;border:1px solid #2b2b2b;border-radius:11px;padding:6px;box-shadow:0 12px 40px rgba(0,0,0,.5);z-index:10}
.menu a{display:flex;align-items:center;gap:9px;padding:7px 9px;border-radius:7px;color:#c9c9c9;font-size:12.5px}
.menu a:hover{background:#262626}
.menu .sep{height:1px;background:#2a2a2a;margin:5px 4px}
.menu .hd{padding:7px 9px 4px;font-size:11px;color:#6e6e6e}
.menu .k{margin-left:auto;color:#6e6e6e;font-size:11px}
.attach-menu{position:absolute;width:200px;background:#1d1d1d;border:1px solid #2c2c2c;border-radius:11px;padding:6px;box-shadow:0 12px 40px rgba(0,0,0,.5);z-index:10}
.attach-menu a{display:flex;align-items:center;gap:9px;padding:7px 9px;border-radius:7px;color:#d9d9d9;font-size:12.5px}
.attach-menu a svg{width:14px;height:14px;color:#8a8a8a}
.attach-menu .k{margin-left:auto;color:#6e6e6e;font-size:11px}
.hoverrow{display:flex;align-items:center;gap:12px;justify-content:flex-end;color:#6e6e6e;font-size:11.5px;padding:2px 8px 0}
.hoverrow .hicon{color:#8a8a8a}
.hovertag{font-size:10px;color:#5e5e5e;border:1px dashed #3a3a3a;border-radius:5px;padding:1px 5px;margin-left:6px}
.browser{flex:1;display:flex;flex-direction:column;border-left:1px solid #1f1f1f;background:#0e0e0e;min-width:0}
.bchrome{height:44px;flex:none;display:flex;align-items:center;gap:8px;padding:0 12px;border-bottom:1px solid #1f1f1f;color:#8a8a8a}
.bchrome .navbtn{color:#8a8a8a;width:24px;height:24px;border-radius:6px;display:flex;align-items:center;justify-content:center}
.bchrome .url{flex:1;display:flex;align-items:center;gap:7px;background:#1a1a1a;border:1px solid #262626;border-radius:8px;padding:5px 10px;font-size:12px;color:#a3a3a3;justify-content:center}
.bview{flex:1;overflow:hidden;position:relative;background:#141414}
.mini-app{display:flex;height:100%;font-size:11px}
.mini-sb{width:120px;flex:none;background:#0d0d0d;border-right:1px solid #1f1f1f;padding:8px;display:flex;flex-direction:column;gap:2px}
.mini-sb .mi{padding:4px 7px;border-radius:6px;color:#a3a3a3;font-size:10.5px}
.mini-sb .mi.on{background:#1c1c1c;color:#e8e8e8}
.mini-main{flex:1;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:14px;padding:20px}
.mini-greet{display:flex;align-items:center;gap:8px;font-family:Georgia,serif;font-size:17px;color:#e8e8e8}
.mini-greet img{width:18px;height:18px;border-radius:50%}
.mini-comp{width:78%;max-width:340px;background:#1d1d1d;border:1px solid #2b2b2b;border-radius:10px;padding:9px 11px;font-size:10.5px}
.mini-comp .row{display:flex;justify-content:space-between;align-items:center;margin-top:8px}
.mini-chip{font-size:9.5px;color:#a3a3a3;background:#262626;border-radius:5px;padding:2px 6px}
.goalbar{display:flex;align-items:center;gap:10px;font-size:12px}
.goal-pill{display:inline-flex;align-items:center;gap:8px;background:#1c1a16;border:1px solid #3d3527;border-radius:9px;padding:5px 10px;color:#e8d9b0;font-size:12px}
.goal-pill .gbtn{width:20px;height:20px;border-radius:6px;background:#262019;border:1px solid #3d3527;display:flex;align-items:center;justify-content:center;color:#c9b890}
.goal-pill .gtime{color:#a89a76;font-variant-numeric:tabular-nums}
.subagent{border:1px solid #2c2c2c;border-left:2px solid #d9a08e;border-radius:10px;padding:10px 12px;background:#171717}
.subagent .hd{display:flex;align-items:center;gap:9px;font-size:12.5px;color:#d9d9d9}
.subagent .sub{margin:8px 0 0 15px;border-left:1px solid #2c2c2c;padding-left:12px;display:flex;flex-direction:column;gap:5px}
.subagent .srow{display:flex;align-items:center;gap:8px;font-size:12px;color:#8a8a8a}
.tpsline{display:flex;align-items:center;gap:14px;color:#6e6e6e;font-size:11px;padding:2px 4px;font-variant-numeric:tabular-nums}
.tpsline .live{color:#4ade80}
.cmdpal{position:absolute;left:0;right:0;bottom:calc(100% + 8px);background:#1c1c1c;border:1px solid #2c2c2c;border-radius:11px;padding:6px;box-shadow:0 -8px 32px rgba(0,0,0,.45)}
.cmdpal .chead{font-size:10.5px;color:#6e6e6e;padding:3px 9px 6px}
.cmdpal a{display:flex;align-items:baseline;gap:10px;padding:6px 9px;border-radius:7px;font-size:12.5px}
.cmdpal a .c{font-family:ui-monospace,Menlo,monospace;color:#8aa8d8;min-width:110px}
.cmdpal a .d{color:#8a8a8a;font-size:12px}
.cmdpal a.sel{background:#262626}
.attach-row{display:flex;gap:8px;margin-bottom:9px}
.atch{display:flex;align-items:center;gap:8px;background:#222;border:1px solid #2e2e2e;border-radius:9px;padding:6px 9px;font-size:11.5px;color:#c9c9c9}
.atch img{width:26px;height:26px;border-radius:6px;object-fit:cover}
.atch .x{color:#6e6e6e;margin-left:2px}
.msgpreview{border:1px dashed #3a3a3a;border-radius:10px;padding:10px 12px;margin-top:10px}
.msgpreview .ph{font-size:10px;color:#6e6e6e;letter-spacing:.06em;text-transform:uppercase;margin-bottom:6px}
.usage-cards{display:flex;gap:10px;margin:10px 0}
.ucard{flex:1;background:#1a1a1a;border:1px solid #262626;border-radius:10px;padding:11px 12px}
.ucard .uh{font-size:12px;color:#c9c9c9;margin-bottom:7px}
.ucard .ubig{font-size:16px;color:#e8e8e8;font-weight:600}
.ucard .us{font-size:10.5px;color:#6e6e6e;font-weight:400}
.ucard .urow{display:flex;justify-content:space-between;font-size:10.5px;color:#8a8a8a;padding:2px 0}
.diff{border:1px solid #262626;border-radius:10px;overflow:hidden;background:#131313;margin-top:4px}
.diff .dh{display:flex;align-items:center;gap:10px;padding:7px 11px;border-bottom:1px solid #222;font-size:11.5px;color:#a3a3a3;font-family:ui-monospace,Menlo,monospace}
.diff .dh .plus{color:#4ade80;font-weight:600}
.diff .dh .minus{color:#f87171;font-weight:600}
.diff .dl{display:flex;align-items:center;font-family:ui-monospace,Menlo,monospace;font-size:11.5px;line-height:1.7;color:#8a8a8a;white-space:pre}
.diff .dl .ln{width:36px;flex:none;color:#4d4d4d;text-align:right;padding-right:8px;font-size:10.5px}
.diff .dl .sg{width:16px;flex:none;text-align:center;color:#6e6e6e}
.diff .dl.add{background:rgba(74,222,128,.07);color:#a9e8c0}
.diff .dl.add .sg{color:#4ade80}
.diff .dl.del{background:rgba(248,113,113,.06);color:#dca9a4}
.diff .dl.del .sg{color:#f87171}
.diff .dl.hunk{background:#181818;color:#7aa2f7;font-size:10.5px}
.diff .more{padding:5px 11px;border-top:1px solid #1f1f1f;font-size:11px;color:#5e5e5e;text-align:center}
.chart{display:flex;align-items:flex-end;gap:4px;height:110px;padding-top:8px;position:relative}
.tip{position:absolute;left:69%;bottom:96px;transform:translateX(-50%);background:#1c1c1c;border:1px solid #2b2b2b;border-radius:7px;padding:6px 9px;font-size:10.5px;color:#c9c9c9;box-shadow:0 8px 24px rgba(0,0,0,.5);white-space:nowrap;z-index:5}
.tip .tt{font-weight:500;color:#e8e8e8;margin-bottom:3px}
.tip .tt span{color:#6e6e6e;font-weight:400;margin-left:5px}
.tip .tr{display:flex;align-items:center;gap:6px;padding:1px 0}
.tip i{display:inline-block;width:6px;height:6px;border-radius:1.5px}
.chart .bar{flex:1;min-width:4px;display:flex;flex-direction:column}
.chart .bar>i{display:block;width:100%}
.chart .bar>i:first-child{border-radius:2px 2px 0 0}
.chart .bar>i:last-child{border-radius:0 0 1px 1px}
.toolpill{display:inline-flex;align-items:center;gap:6px;border:1px solid #2e2e2e;border-radius:8px;padding:4px 11px;font-size:12px;color:#c9c9c9;background:#1c1c1c}
.toolpill svg{color:#8a8a8a}
.iconchip{width:26px;height:26px;border-radius:8px;background:#1c1c1c;border:1px solid #2e2e2e;display:flex;align-items:center;justify-content:center;color:#a3a3a3}
.effpop{position:absolute;width:272px;background:#1c1c1c;border:1px solid #2c2c2c;border-radius:12px;padding:14px;box-shadow:0 14px 44px rgba(0,0,0,.55);z-index:10}
.effpop .eh{display:flex;align-items:center;justify-content:space-between;font-size:13px;color:#a3a3a3}
.effpop .eh b{color:#c8b7f2;font-weight:600}
.effpop .elbl{display:flex;justify-content:space-between;font-size:11px;color:#7a7a7a;margin:12px 0 4px}
.efftrack{position:relative;height:24px;border-radius:4px;background:rgba(255,255,255,.10)}
.efftrack .fillg{position:absolute;inset:0;border-radius:4px;background:linear-gradient(90deg,rgba(255,255,255,.26) 0%,rgba(160,150,235,.20) 55%,rgba(160,150,235,.32) 100%)}
.efftrack .gridfx{position:absolute;inset:0;border-radius:4px;background-image:radial-gradient(circle at 1.6px 1.6px,rgba(216,200,250,.9) 1.15px,transparent 1.3px);background-size:5px 5px;-webkit-mask-image:linear-gradient(90deg,transparent 0%,#000 45%);mask-image:linear-gradient(90deg,transparent 0%,#000 45%)}
.efftrack .stop{position:absolute;top:10.5px;width:3px;height:3px;border-radius:50%;background:rgba(255,255,255,.28);margin-left:-1.5px}
.efftrack .handle{position:absolute;top:0;right:0;width:20px;height:24px;border-radius:4px;background:linear-gradient(180deg,#fbfaf6,#eae7df);box-shadow:0 0 0 1px rgba(0,0,0,.22),0 1px 2px rgba(0,0,0,.45),0 0 22px rgba(160,150,235,.5)}
.modelmenu{position:absolute;width:216px;background:#1d1d1d;border:1px solid #2c2c2c;border-radius:11px;padding:6px;box-shadow:0 12px 40px rgba(0,0,0,.5);z-index:10}
.modelmenu .mh{font-size:10.5px;color:#6e6e6e;padding:4px 9px 6px}
.modelmenu a{display:flex;align-items:center;gap:9px;padding:7px 9px;border-radius:7px;color:#c9c9c9;font-size:12.5px}
.modelmenu a .m{color:#6e6e6e;font-size:11px;margin-left:auto}
.mcp-row{display:flex;align-items:center;justify-content:space-between;padding:8px 0;border-bottom:1px solid #202020;font-size:12.5px}
.mcp-row .n{color:#e8e8e8}
.mcp-row .d{color:#7a7a7a;font-size:11.5px}
"""

def svg(name):
    icons = {
        "plus": '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 5v14M5 12h14"/></svg>',
        "search": '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="11" cy="11" r="7"/><path d="m21 21-4.3-4.3"/></svg>',
        "chat": '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/></svg>',
        "box": '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z"/></svg>',
        "clock": '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 3"/></svg>',
        "sliders": '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3M1 14h6M9 8h6M17 16h6"/></svg>',
        "db": '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><ellipse cx="12" cy="5" rx="8" ry="3"/><path d="M4 5v14c0 1.7 3.6 3 8 3s8-1.3 8-3V5"/><path d="M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3"/></svg>',
        "key": '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="m21 2-2 2m-7.6 7.6a5.5 5.5 0 1 1-7.8 7.8 5.5 5.5 0 0 1 7.8-7.8zm0 0L19 4m3 3-3-3"/></svg>',
        "download": '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3"/></svg>',
        "gear": '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="3"/><path d="M12 1v4m0 14v4M4.2 4.2l2.8 2.8m10 10 2.8 2.8M1 12h4m14 0h4M4.2 19.8l2.8-2.8m10-10 2.8-2.8"/></svg>',
        "home": '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="m3 9 9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/></svg>',
        "monitor": '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="2" y="3" width="20" height="14" rx="2"/><path d="M8 21h8m-4-4v4"/></svg>',
        "bookmark": '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="m19 21-7-4-7 4V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v16z"/></svg>',
        "shield": '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/></svg>',
        "dots": '<svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.5"/><circle cx="12" cy="12" r="1.5"/><circle cx="19" cy="12" r="1.5"/></svg>',
        "help": '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="9"/><path d="M9.1 9a3 3 0 0 1 5.8 1c0 2-3 3-3 3m.1 4h.01"/></svg>',
        "pin": '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 17v5M9 10.8a2 2 0 0 0-1-1.7L4 7V4h16v3l-4 2.1a2 2 0 0 0-1 1.7V14l3 3H6l3-3z"/></svg>',
        "send": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4"><path d="M12 19V5m-7 7 7-7 7 7"/></svg>',
        "attach": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="m21.4 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l8.57-8.57A4 4 0 1 1 18 8.84l-8.59 8.57a2 2 0 0 1-2.83-2.83l8.49-8.48"/></svg>',
        "terminal": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="m4 17 6-6-6-6M12 19h8"/></svg>',
        "check": '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="#4ade80" stroke-width="3"><path d="M20 6 9 17l-5-5"/></svg>',
        "circle": '<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="#6e6e6e" stroke-width="2"><circle cx="12" cy="12" r="9"/></svg>',
        "half": '<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="#fac454" stroke-width="2"><circle cx="12" cy="12" r="9"/><path d="M12 3v18" fill="#fac454" fill-opacity=".25"/></svg>',
        "x": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M18 6 6 18M6 6l12 12"/></svg>',
        "folder": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/></svg>',
        "globe": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a15 15 0 0 1 0 18 15 15 0 0 1 0-18z"/></svg>',
        "cpu": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="4" y="4" width="16" height="16" rx="2"/><rect x="9" y="9" width="6" height="6"/><path d="M9 1v3m6-3v3M9 20v3m6-3v3M1 9h3m-3 6h3M20 9h3m-3 6h3"/></svg>',
        "warn": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="#fac454" stroke-width="2"><path d="m12 3 10 18H2z"/><path d="M12 10v4m0 3h.01"/></svg>',
        "file": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><path d="M14 2v6h6"/></svg>',
        "share": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M7 17 17 7M8 7h9v9"/></svg>',
        "retry": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 12a9 9 0 1 1-3-6.7L21 8"/><path d="M21 3v5h-5"/></svg>',
        "edit": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M17 3a2.8 2.8 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5z"/></svg>',
        "copy": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10"/></svg>',
        "puzzle": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M14 7h3a1 1 0 0 1 1 1v3m0 4v3a1 1 0 0 1-1 1h-3m-4 0H7a1 1 0 0 1-1-1v-3m0-4V8a1 1 0 0 1 1-1h3m2-3v3m-2 11v3"/></svg>',
        "back": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M19 12H5m7 7-7-7 7-7"/></svg>',
        "fwd": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M5 12h14m-7-7 7 7-7 7"/></svg>',
        "reload": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 12a9 9 0 1 1-3-6.7L21 8"/><path d="M21 3v5h-5"/></svg>',
        "lock": '<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="#6e6e6e" stroke-width="2"><rect x="4" y="11" width="16" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/></svg>',
        "external": '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6M15 3h6v6M10 14 21 3"/></svg>',
        "pause": '<svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor"><rect x="5" y="4" width="5" height="16" rx="1.2"/><rect x="14" y="4" width="5" height="16" rx="1.2"/></svg>',
        "stop": '<svg width="9" height="9" viewBox="0 0 24 24" fill="currentColor"><rect x="5" y="5" width="14" height="14" rx="2"/></svg>',
        "agent": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="8" r="4"/><path d="M4 21v-1a7 7 0 0 1 14 0v1"/><path d="m19 8 3-2-3-2-3 2z"/></svg>',
        "flag": '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M4 15s1-1 4-1 5 2 8 2 4-1 4-1V3s-1 1-4 1-5-2-8-2-4 1-4 1z"/><path d="M4 22v-7"/></svg>',
        "list": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M8 6h13M8 12h13M8 18h13"/><circle cx="3.5" cy="6" r="1" fill="currentColor"/><circle cx="3.5" cy="12" r="1" fill="currentColor"/><circle cx="3.5" cy="18" r="1" fill="currentColor"/></svg>',
        "spark": '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M13 2 4.5 12.5H11l-1 9.5L19.5 11H13z"/></svg>',
        "layers": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="m12 2 9 5-9 5-9-5z"/><path d="m3 12 9 5 9-5"/><path d="m3 17 9 5 9-5"/></svg>',
    }
    return icons.get(name, "")

def sidebar(active=""):
    recents = [
        ("Implement desktop UI preview with Vite HMR", "code · 3 min", True),
        ("Markdown, Math & Mermaid Budget Showcase", "plan · 1 h", False),
        ("规划基于 Tauri 2 的独立 Desktop 安装方案", "plan · 2 h", False),
        ("导出会话到 HANDOFF 文档", "work · 5 h", False),
        ("审查 capability-roadmap 未完成项", "plan · yesterday", False),
    ]
    items = [
        ("plus", "New session", "new"),
        ("search", "Search", "search"),
        ("chat", "Sessions", "sessions"),
        ("db", "Memory", "memory"),
        ("key", "MCP & Creds", "mcp"),
        ("download", "Export", "export"),
        ("sliders", "Customize", "customize"),
    ]
    nav = "".join(
        f'<a>{svg(i)}<span>{t}</span></a>' for i, t, _ in items
    )
    recent_html = "".join(
        f'<a class="{"sel" if s else ""}">{t}<span style="display:block;font-size:10.5px;color:#6b6b6b">{m}</span></a>'
        for t, m, s in recents
    )
    return f'''<div class="sidebar">
      <div class="nav">
        <a style="background:#1c1c1c;color:#e8e8e8">{svg("plus")}<span style="font-weight:500">New</span></a>
        {"".join(f'<a>{svg(i)}<span>{t}</span></a>' for i,t,_ in items[1:])}
      </div>
      <div class="section-label">Recents <span>⌄</span></div>
      <div class="recent">{recent_html}</div>
      <a style="padding:6px 10px;color:#6e6e6e;font-size:12px">View all</a>
      <div class="user-chip"><img src="{ICON}"><span>ariakage · anthropic-official</span></div>
    </div>'''

def sess_col(width="248px", sel="Implement desktop UI preview with Vite HMR", meta="1 minute ago"):
    def it(t, m, s=False):
        return f'<div class="sess-item{" sel" if s else ""}"><span class="t">{t}</span><span class="meta">{m}</span></div>'
    return f'''<div class="sessions-col" style="width:{width}">
  <div style="display:flex;align-items:center;justify-content:space-between;padding:2px 4px 10px"><span style="font-size:14px;font-weight:600;color:#e8e8e8">Sessions</span><span class="rbtn" style="width:26px;height:26px">{svg("dots")}</span></div>
  <a style="display:flex;align-items:center;gap:8px;color:#d9a08e;padding:6px 8px;font-size:13px">{svg("plus")}New session</a>
  <div class="section-label" style="margin-top:10px">Pinned 1</div>
  <div class="sess-item"><span class="t">{svg("pin")} Hi, I'm Soyo…</span><span class="meta" style="color:#d9a08e">Answer questions</span></div>
  <div class="section-label" style="margin-top:12px">{svg("folder")} saya-harness <span style="font-weight:400">3</span></div>
  {it("Implement desktop UI preview with Vite HMR", "goal active" if meta == "goal active" else meta, sel == "Implement desktop UI preview with Vite HMR")}
  {it("Markdown, Math & Mermaid showcase", "27 minutes ago")}
  {it("规划基于 Tauri 2 的安装方案", "2 hours ago")}
  <div class="section-label" style="margin-top:12px">{svg("folder")} AkaMemo <span style="font-weight:400">2</span></div>
  {it("项目 AGENTS.md 与文档规范 (fork)", "2 hours ago", sel == "项目 AGENTS.md 与文档规范 (fork)")}
  {it("确认仓库贡献指南 / PR 模板", "3 hours ago")}
  <div class="section-label" style="margin-top:12px">{svg("folder")} ai-gateway <span style="font-weight:400">1</span></div>
  {it("导出会话到 HANDOFF 文档", "5 hours ago")}
</div>'''

# ---------- context ring helper ----------
def ctxring(pct=0.5, warn=None):
    """Circular context-window indicator. pct 0..1; color auto: accent<75%, amber<92%, red>=92%."""
    import math
    r, c = 6.0, 2 * math.pi * 6.0
    if warn is None:
        warn = "#d9a08e" if pct < 0.75 else ("#fac454" if pct < 0.92 else "#f87171")
    dash = max(1.0, c * pct)
    return (f'<span style="display:inline-flex;align-items:center;margin:0 1px" title="Context window">'
            f'<svg width="16" height="16" viewBox="0 0 16 16">'
            f'<circle cx="8" cy="8" r="{r}" fill="none" stroke="#3a3a3a" stroke-width="2.2"/>'
            f'<circle cx="8" cy="8" r="{r}" fill="none" stroke="{warn}" stroke-width="2.2" '
            f'stroke-linecap="round" stroke-dasharray="{dash:.1f} {c:.1f}" transform="rotate(-90 8 8)"/>'
            f'</svg></span>')


def rail(active="chat"):
    items = ["home", "chat", "search", "db", "shield", "gear"]
    btns = "".join(
        f'<div class="rbtn{" active" if i == active else ""}">{svg(i)}</div>' for i in items
    )
    return f'<div class="rail">{btns}<div style="margin-top:auto" class="rbtn">{svg("help")}</div><div class="rbtn" style="margin-top:2px"><img src="{ICON}" style="width:22px;height:22px;border-radius:50%"></div></div>'

def composer_bar(mode="CODE", placeholder="Ask Saya to analyze, edit, or run commands…"):
    return f'''<div class="composer-bar"><div class="inner">
      <div class="composer" style="width:100%">
        <div class="ta ph">{placeholder}</div>
        <div class="row">
          <div style="display:flex;gap:2px"><span class="chip">{svg("plus")}</span></div>
          <div style="display:flex;align-items:center;gap:2px">
            <span class="chip"><span class="dot" style="background:#4ade80"></span>{mode}</span>
            <span class="chip">auto</span>
            <span class="chip">claude-sonnet-4-5 · high</span>
            {ctxring(0.5)}<span class="send">{svg("send")}</span>
          </div>
        </div>
      </div>
      <div class="kbd-hint"><span>Enter send</span><span>Shift+Enter newline</span><span>Empty Tab cycle mode</span><span>Esc cancel</span></div>
    </div></div>'''

def page(body, extra_head=""):
    return f"<!DOCTYPE html><html><head><meta charset='utf-8'><style>{CSS}</style>{extra_head}</head><body>{body}</body></html>"

# ---------- 01 Home ----------
home = f'''<div class="app">{sidebar()}
<div class="main">
  <div class="content">
    <div class="center-home">
      <div class="greet"><img src="{ICON}">Good morning, ariakage.</div>
      <div class="composer" style="margin-top:34px">
        <div class="ta ph">How can I help you today?</div>
        <div class="row">
          <span class="chip">{svg("plus")}</span>
          <div style="display:flex;align-items:center;gap:2px">
            <span class="chip">claude-sonnet-4-5 <span style="color:#6e6e6e">High</span></span>
            {ctxring(0.12)}<span class="send off">{svg("send")}</span>
          </div>
        </div>
      </div>
      <div class="chip" style="margin-top:10px;background:#1d1d1d;border:1px solid #2b2b2b;padding:8px 12px;border-radius:0 0 12px 12px;border-top:none">{svg("folder")} Project or folder</div>
      <div class="ideas">
        <div class="lbl">Ideas for you</div>
        <a><span class="ib">{svg("clock")}</span>Summarize today's sessions into a handoff note</a>
        <a><span class="ib">{svg("box")}</span>Audit MCP servers and credential scopes</a>
        <a><span class="ib">{svg("sliders")}</span>Customize Saya for this workspace</a>
      </div>
    </div>
    {composer_bar("WORK") if False else ""}
  </div>
</div></div>'''

# ---------- 02 Session conversation ----------
session = f'''<div class="app">{rail("chat")}
{sess_col()}
<div class="main">
  <div class="topbar">
    <div style="display:flex;align-items:center;gap:10px">
      <span>{svg("chat")} Implement desktop UI preview with Vite HMR</span>
      <span style="color:#6b6b6b">· saya-harness</span>
    </div>
    <div style="display:flex;gap:4px;align-items:center">
      <span class="badge green">CODE</span>
      <span class="badge plain">auto</span>
      <span class="chip">{svg("dots")}</span>
    </div>
  </div>
  <div class="content">
    <div class="thread">
      <div style="display:flex;flex-direction:column">
        <div class="msg-user">Check the runtime boundary for soyo-desktop.</div>
        <div class="hoverrow"><span>Oct 28</span><span class="hicon">{svg("share")}</span><span class="hicon">{svg("retry")}</span><span class="hicon">{svg("edit")}</span><span class="hicon">{svg("copy")}</span><span class="hovertag">on hover</span></div>
      </div>
      <div class="thinking"><span class="chev">›</span><span>Thought for 8 s</span><span class="badge plain">auto-collapsed</span></div>
      <div class="toolcard">{svg("terminal")}<span class="tname">read_file</span><span class="badge green">done</span><span style="flex:1"></span><span style="color:#6e6e6e">crates/soyo-app/src/lib.rs · 90 lines ›</span></div>
      <div class="md md-plain">
        <p>In-process linking directly invokes <code>soyo_app::spawn_build_runtime</code> over <code>Channel&lt;DesktopStreamEnvelope&gt;</code>:</p>
        <ul><li>Blocking SQLite reads stay isolated in <code>offload_boundary</code>.</li><li>No remote assets or keyring plugins are loaded.</li></ul>
        <div class="codeblock"><span class="k">pub async fn</span> <span class="f">cancel_run</span>(&amp;<span class="k">self</span>) -&gt; <span class="k">bool</span> {{<br>&nbsp;&nbsp;&nbsp;&nbsp;<span class="k">self</span>.runtime.cancel_run()<br>}}</div>
        <p>All events stream through the bounded channel with backpressure — the UI only consumes <a href="#">schema v1 envelopes</a>.</p>
      </div>
      <div class="toolcard">{svg("edit")}<span class="tname">edit_file</span><span class="badge green">done</span><span style="flex:1"></span><span style="color:#6e6e6e">desktop/src/App.tsx</span><span style="color:#4ade80;font-weight:600"> +128</span><span style="color:#f87171;font-weight:600"> -12</span></div>
      <div class="diff">
        <div class="dh"><span style="color:#c9c9c9">desktop/src/App.tsx</span><span class="plus">+128</span><span class="minus">-12</span><span style="flex:1"></span><span style="color:#5e5e5e">2 hunks ›</span></div>
        <div class="dl hunk"><span class="ln"></span><span class="sg"></span><span>@@ -855,7 +855,10 @@ export function App()</span></div>
        <div class="dl"><span class="ln">856</span><span class="sg"> </span><span>const [copiedId, setCopiedId] = useState&lt;string | null&gt;(null)</span></div>
        <div class="dl add"><span class="ln"></span><span class="sg">+</span><span>const [editDraft, setEditDraft] = useState&lt;Draft | null&gt;(null)</span></div>
        <div class="dl"><span class="ln">857</span><span class="sg"> </span><span>const [forceHome, setForceHome] = useState(false)</span></div>
        <div class="dl hunk"><span class="ln"></span><span class="sg"></span><span>@@ -912,10 +915,14 @@ function MessageRow()</span></div>
        <div class="dl del"><span class="ln">913</span><span class="sg">-</span><span>&lt;Pencil className="h-3 w-3 opacity-50" /&gt;</span></div>
        <div class="dl add"><span class="ln"></span><span class="sg">+</span><span>&lt;button onClick={{() =&gt; setEditDraft(msg.text)}} title="Edit"&gt;</span></div>
        <div class="dl add"><span class="ln"></span><span class="sg">+</span><span>&nbsp;&nbsp;&lt;Pencil className="h-3 w-3" /&gt;</span></div>
        <div class="dl add"><span class="ln"></span><span class="sg">+</span><span>&lt;/button&gt;</span></div>
        <div class="dl"><span class="ln">916</span><span class="sg"> </span><span>&lt;Copy className="h-3 w-3" /&gt;</span></div>
        <div class="more">122 unchanged lines hidden · expand ›</div>
      </div>
      <div class="msg-user">Run git status and summarize the changes.</div>
      <div class="thinking"><span class="chev" style="display:inline-block;transform:rotate(90deg)">›</span><span>Thinking</span></div>
      <div class="thinking-body">The user wants a quick summary — check git status first, then group changes by file type. No need for full test suite yet.</div>
      <div class="toolcard">{svg("terminal")}<span class="tname">exec</span><span class="badge amber">running</span><span style="flex:1"></span><span style="color:#6e6e6e">$ git status -s</span></div>
    </div>
    <div class="composer-bar"><div class="inner">
      <div class="composer" style="width:100%">
        <div class="ta ph">Type a message or / for commands…</div>
        <div class="row">
          <div style="display:flex;gap:6px;align-items:center"><span class="chip">{svg("plus")}</span><span class="chip">{svg("attach")}</span><span class="toolpill">{svg("globe")}Web search</span><span class="iconchip">{svg("list")}</span></div>
          <div style="display:flex;align-items:center;gap:2px">
            <span class="chip"><span class="dot" style="background:#fac454"></span>CODE</span>
            <span class="chip" style="color:#8aa8d8">auto</span>
            <span class="chip">claude-sonnet-4-5 Max</span>
            <span class="chip" style="border:1px solid #2e2e2e">Steer ⌄</span>
            {ctxring(0.62)}<span class="send" title="Stop">{svg("stop")}</span>
          </div>
        </div>
      </div>
    </div></div>
  </div>
</div>
<div class="panel">
  <div>
    <h4>Runtime <span style="color:#4ade80">●</span></h4>
    <div class="prow"><span class="k">Profile</span><span>anthropic-official</span></div>
    <div class="prow"><span class="k">Model</span><span>claude-sonnet-4-5</span></div>
    <div class="prow"><span class="k">Sandbox</span><span>seatbelt</span></div>
    <div class="prow"><span class="k">Workspace</span><span style="font-family:ui-monospace,Menlo,monospace;font-size:11px">~/saya-harness</span></div>
  </div>
  <div>
    <h4>Context window <span style="color:#6e6e6e">187.4k ›</span></h4>
    <div class="ctxbar"><div style="width:55%;background:#d9a08e"></div><div style="width:12%;background:#e07b3f"></div><div style="width:10%;background:#3fbf7f"></div><div style="width:5%;background:#bf9f3f"></div><div style="width:18%;background:#3a3a3a"></div></div>
    <div class="legend">
      <div class="li"><span class="l"><span class="sw" style="background:#d9a08e"></span>Messages</span><span>103.1k · 55%</span></div>
      <div class="li"><span class="l"><span class="sw" style="background:#e07b3f"></span>Skills</span><span>22.5k · 12%</span></div>
      <div class="li"><span class="l"><span class="sw" style="background:#3fbf7f"></span>MCP tools</span><span>18.7k · 10%</span></div>
      <div class="li"><span class="l"><span class="sw" style="background:#bf9f3f"></span>System prompt</span><span>9.4k · 5%</span></div>
      <div class="li"><span class="l"><span class="sw" style="background:#3a3a3a"></span>Free space</span><span>33.7k · 18%</span></div>
    </div>
  </div>
  <div>
    <h4>Tasks <span style="color:#6e6e6e">1/3</span></h4>
    <div style="display:flex;flex-direction:column;gap:6px;font-size:12px;margin-top:6px">
      <div style="display:flex;gap:8px;color:#6e6e6e;text-decoration:line-through">{svg("check")}<span>Wire Channel streaming</span></div>
      <div style="display:flex;gap:8px;color:#fac454">{svg("half")}<span>Verify Streamdown + Mermaid fallback</span></div>
      <div style="display:flex;gap:8px;color:#a3a3a3">{svg("circle")}<span>Approval dialog round-trip</span></div>
    </div>
  </div>
  <div>
    <h4>Outputs <span style="color:#6e6e6e">3</span></h4>
    <div style="display:flex;flex-direction:column;gap:4px;font-size:12px;margin-top:6px">
      <div style="display:flex;gap:8px;color:#a3a3a3">{svg("file")}<span>desktop/src/App.tsx</span></div>
      <div style="display:flex;gap:8px;color:#a3a3a3">{svg("file")}<span>src-tauri/src/commands.rs</span></div>
      <div style="display:flex;gap:8px;color:#a3a3a3">{svg("file")}<span>bindings.ts</span></div>
    </div>
  </div>
</div>
</div>'''

# ---------- 03 Approval ----------
approval = f'''<div class="app">{rail("chat")}
{sess_col(meta="Manual Approval")}
<div class="main">
  <div class="topbar">
    <div style="display:flex;align-items:center;gap:10px"><span>{svg("chat")} Implement desktop UI preview…</span><span style="color:#6b6b6b">· saya-harness</span></div>
    <div style="display:flex;gap:4px"><span class="badge green">CODE</span><span class="badge amber">Manual Approval</span></div>
  </div>
  <div class="content">
    <div class="thread">
      <div class="msg-user">Run the workspace verification tests.</div>
      <div class="approval">
        <div class="ttl"><span style="display:flex;align-items:center;gap:8px">{svg("warn")} Approval required</span><span class="badge green">CODE</span></div>
        <div class="kv">
          <span class="k">Tool</span><span class="v">exec</span>
          <span class="k">Action</span><span class="v" style="color:#7aa2f7">shell.exec</span>
          <span class="k">Call</span><span class="v">call_exec_01</span>
          <span class="k">Workspace</span><span class="v">~/saya-harness</span>
        </div>
        <div style="font-size:12.5px;color:#c9c9c9">Run workspace verification tests with cargo</div>
        <div class="cmd">$ cargo test -p soyo-core --locked</div>
        <div style="display:flex;align-items:center;justify-content:space-between">
          <span style="font-size:11px;color:#6e6e6e">Press <b>Y</b> to allow or <b>N</b> to deny</span>
          <div class="btnrow"><span class="btn deny">Deny (N)</span><span class="btn allow">Allow (Y)</span></div>
        </div>
      </div>
      <div class="msg-sys">Paused · waiting for your decision — the run resumes after you reply.</div>
    </div>
    <div class="composer-bar"><div class="inner">
      <div class="composer" style="width:100%">
        <div class="ta ph">Steer the run, or reply above…</div>
        <div class="row">
          <span class="chip">{svg("plus")}</span>
          <div style="display:flex;gap:2px;align-items:center">
            <span class="chip"><span class="dot" style="background:#4ade80"></span>CODE</span>
            <span class="chip" style="color:#fac454">Manual Approval</span>
            <span class="chip">claude-sonnet-4-5 Max</span>
            <span class="chip" style="border:1px solid #2e2e2e">Steer ⌄</span>
            {ctxring(0.74)}<span class="send off">{svg("send")}</span>
          </div>
        </div>
      </div>
    </div></div>
  </div>
</div></div>'''

# ---------- 04 Settings ----------
settings = f'''<div class="app">{sidebar()}
<div class="main"><div class="content" style="align-items:center;justify-content:center">
  <div class="overlay" style="position:absolute">
    <div class="dialog" style="width:680px;height:520px">
      <div class="dnav">
        <a class="sel">{svg("gear")}General</a>
        <a>{svg("sliders")}Appearance</a>
        <a>{svg("cpu")}Model &amp; Profiles</a>
        <a>{svg("shield")}Permissions</a>
        <a>{svg("key")}MCP &amp; Credentials</a>
        <a>{svg("db")}Memory</a>
        <a>{svg("box")}Modules</a>
        <a>{svg("download")}Import &amp; export</a>
        <a>{svg("monitor")}System</a>
      </div>
      <div class="dbody">
        <h3>Runtime &amp; Modules</h3>
        <div class="field"><div class="fl">Workspace directory</div><div class="fv"><span style="font-family:ui-monospace,Menlo,monospace;font-size:12px">~/saya-harness</span><span class="chip" style="border:1px solid #2b2b2b">Apply</span></div></div>
        <div style="display:flex;gap:10px">
          <div class="field" style="flex:1"><div class="fl">Model profile</div><div class="fv"><span>anthropic-official (claude-so…</span><span>⌄</span></div></div>
          <div class="field" style="flex:1"><div class="fl">Thinking effort</div><div class="fv"><span>high</span><span>⌄</span></div></div>
          <div class="field" style="flex:1"><div class="fl">Sandbox backend</div><div class="fv"><span>seatbelt</span><span>⌄</span></div></div>
        </div>
        <div class="field"><div class="fl" style="display:flex;justify-content:space-between;align-items:center"><span>Auto-compact (80%)</span><span class="badge green">On</span></div><div class="fv"><span style="font-family:ui-monospace,Menlo,monospace;font-size:12px">160000</span><span class="chip" style="border:1px solid #2b2b2b">Set trigger</span></div></div>
        <div class="field"><div class="fl">Default work mode</div><div class="seg"><span>PLAN</span><span>BUILD</span><span class="on">CODE</span><span>WORK</span></div></div>
        <div class="field"><div class="fl">Allow bypass permissions mode</div>
          <div style="font-size:11.5px;color:#7a7a7a;margin:4px 0 8px;line-height:1.5">Bypass skips path checks, model review, and human confirmation. PLAN, explicit deny and hard backend limits still apply.</div>
          <div class="switch"></div>
        </div>
      </div>
    </div>
  </div>
  <div class="thread" style="filter:brightness(.5)">
      <div class="msg-user">Check the runtime boundary for soyo-desktop.</div>
      <div class="md md-plain"><p>In-process linking directly invokes <code>spawn_build_runtime</code>…</p></div>
  </div>
</div></div></div>'''

# ---------- 05 User menu ----------
menu = f'''<div class="app">{sidebar()}
<div class="main"><div class="content">
  <div class="center-home">
    <div class="greet"><img src="{ICON}">You're here!</div>
    <div class="composer" style="margin-top:34px"><div class="ta ph">How can I help you today?</div>
      <div class="row"><span class="chip">{svg("plus")}</span>
      <div style="display:flex;gap:2px;align-items:center"><span class="chip">claude-sonnet-4-5 <span style="color:#6e6e6e">High</span></span>{ctxring(0.34)}<span class="send off">{svg("send")}</span></div></div>
    </div>
  </div>
</div></div>
<div class="menu">
  <div class="hd">ariakage<br><span style="color:#555">anthropic-official</span></div>
  <a>{svg("gear")}Settings</a>
  <a>{svg("globe")}Language</a>
  <a>{svg("cpu")}Model providers</a>
  <a>{svg("file")}View changelog</a>
  <a>{svg("help")}Learn more</a>
  <div class="sep"></div>
  <a>{svg("x")}Sign out</a>
</div>
</div>'''

# ---------- 06 Sessions home (rail + list + empty main) ----------
sessions_view = f'''<div class="app">{rail("chat")}
{sess_col(width="260px", sel="项目 AGENTS.md 与文档规范 (fork)", meta="2 hours ago")}
<div class="main">
  <div class="topbar"><div style="display:flex;gap:8px;align-items:center"><span>项目 AGENTS.md 与文档规范 (fork)</span><span style="color:#6b6b6b">⌄</span></div><div style="display:flex;gap:4px"><span class="chip">{svg("sliders")}</span><span class="chip">{svg("dots")}</span></div></div>
  <div class="content">
    <div class="thread">
      <div class="md md-plain" style="max-width:760px">
        <h3 style="margin-top:0">合并结果</h3>
        <div style="display:grid;grid-template-columns:110px 1fr;gap:6px 14px;font-size:12.5px">
          <span style="color:#7a7a7a">PR</span><span style="color:#8aa8d8">Ariakage/AkaMemo#1</span>
          <span style="color:#7a7a7a">状态</span><span>MERGED</span>
          <span style="color:#7a7a7a">合并方式</span><span>Squash merge</span>
          <span style="color:#7a7a7a">变更规模</span><span>39 个文件、5920 行新增</span>
        </div>
        <h3>当前仓库状态</h3>
        <ul><li>GitHub main 已包含全部文档与工程配置</li><li>本地 main 与 origin/main 同步，工作区干净</li></ul>
        <div class="codeblock"><span class="c">cda3bec</span> docs(docs)：建立项目规范与文档体系<br><span class="c">cad6b75</span> chore(build)：初始化仓库</div>
      </div>
    </div>
    <div class="composer-bar"><div class="inner">
      <div class="composer" style="width:100%"><div class="ta ph">Type / for commands</div>
        <div class="row"><div style="display:flex;gap:2px"><span class="chip">{svg("plus")}</span></div>
        <div style="display:flex;gap:2px;align-items:center"><span class="chip">claude-sonnet-4-5 Max</span><span class="chip" style="color:#8aa8d8">Bypass permissions</span>{ctxring(0.47)}<span class="send off">{svg("send")}</span></div></div>
      </div>
    </div></div>
  </div>
</div>
</div>'''

# ---------- 07 Composer attach menu ----------
attach_menu = f'''<div class="app">{sidebar()}
<div class="main"><div class="content">
  <div class="center-home">
    <div class="greet"><img src="{ICON}">Good evening, ariakage.</div>
    <div style="position:relative;margin-top:34px">
      <div class="composer">
        <div class="ta ph">Type / for skills</div>
        <div class="row">
          <span class="chip" style="background:#2a2a2a;border:1px solid #333">{svg("plus")}</span>
          <div style="display:flex;align-items:center;gap:2px">
            <span class="chip">claude-sonnet-4-5 <span style="color:#6e6e6e">High</span></span>
            {ctxring(0.25)}<span class="send off">{svg("send")}</span>
          </div>
        </div>
      </div>
      <div class="attach-menu" style="left:-8px;top:54px">
        <a>{svg("attach")}Add files or photos<span class="k">⌘U</span></a>
        <div class="sep" style="height:1px;background:#2a2a2a;margin:4px"></div>
        <a>{svg("file")}Skills<span class="k">›</span></a>
        <a>{svg("layers")}Connectors<span class="k">›</span></a>
        <a>{svg("puzzle")}Add plugins</a>
      </div>
    </div>
    <div class="chip" style="margin-top:120px;color:#6e6e6e">{svg("clock")} Send me a daily briefing</div>
  </div>
</div></div></div>'''

# ---------- 08 Session + browser preview ----------
browser_preview = f'''<div class="app">{rail("chat")}
{sess_col(width="230px", meta="live preview")}
<div class="main">
  <div class="topbar">
    <div style="display:flex;align-items:center;gap:10px"><span>{svg("chat")} Implement desktop UI preview…</span><span style="color:#6b6b6b">· saya-harness</span></div>
    <div style="display:flex;gap:4px;align-items:center"><span class="badge green">CODE</span><span class="badge plain">auto</span><span class="chip">{svg("dots")}</span></div>
  </div>
  <div class="content" style="flex-direction:row">
    <div style="flex:1.15;display:flex;flex-direction:column;min-width:0">
      <div class="thread">
        <div class="msg-user">Spin up the Vite preview and check the new theme.</div>
        <div class="toolcard">{svg("terminal")}<span class="tname">exec</span><span class="badge green">done</span><span style="flex:1"></span><span style="color:#6e6e6e">$ pnpm dev --port 5173</span></div>
        <div class="toolcard">{svg("globe")}<span class="tname">browser.open</span><span class="badge green">done</span><span style="flex:1"></span><span style="color:#6e6e6e">localhost:5173 ›</span></div>
        <div class="md md-plain"><p>Preview is live on the right — the sidebar, greeting and composer all render under the new theme. Hot reload is active, so edits in <code>src/</code> appear instantly.</p></div>
      </div>
      <div class="composer-bar" style="padding:10px 18px 14px"><div class="inner">
        <div class="composer" style="width:100%">
          <div class="ta" style="color:#e8e8e8">Attach a screenshot of the preview</div>
          <div class="row"><span class="chip">{svg("plus")}</span>
            <div style="display:flex;gap:2px;align-items:center"><span class="chip"><span class="dot" style="background:#4ade80"></span>CODE</span><span class="chip">claude-sonnet-4-5 Max</span>{ctxring(0.55)}<span class="send">{svg("send")}</span></div>
          </div>
        </div>
      </div></div>
    </div>
    <div class="browser">
      <div class="bchrome">
        <span class="navbtn">{svg("back")}</span><span class="navbtn">{svg("fwd")}</span><span class="navbtn">{svg("reload")}</span>
        <span class="url">{svg("lock")} localhost:5173</span>
        <span class="navbtn">{svg("external")}</span><span class="navbtn">{svg("x")}</span>
      </div>
      <div class="bview">
        <div class="mini-app">
          <div class="mini-sb">
            <div class="mi on">+ New</div><div class="mi">Search</div><div class="mi">Sessions</div><div class="mi">Memory</div><div class="mi">Customize</div>
          </div>
          <div class="mini-main">
            <div class="mini-greet"><img src="{ICON}">Good morning, ariakage.</div>
            <div class="mini-comp">
              <div style="color:#6e6e6e">How can I help you today?</div>
              <div class="row"><span class="mini-chip">+</span><span style="display:flex;gap:4px;align-items:center"><span class="mini-chip">claude-sonnet-4-5</span><span style="width:18px;height:18px;border-radius:50%;background:#d9a08e;display:flex;align-items:center;justify-content:center;color:#1c120e">{svg("send")}</span></span></div>
            </div>
            <div style="display:flex;flex-direction:column;gap:4px;width:78%;max-width:340px">
              <div class="mi" style="color:#6e6e6e;font-size:9.5px;padding:2px 6px">Ideas for you</div>
              <div class="mi" style="background:#1a1a1a;border-radius:6px;padding:5px 8px;color:#bdbdbd">Summarize today's sessions</div>
              <div class="mi" style="background:#1a1a1a;border-radius:6px;padding:5px 8px;color:#bdbdbd">Audit MCP credentials</div>
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>
</div></div>'''

# ---------- 09 Subagent + Goal ----------
subagent_frame = f'''<div class="app">{rail("chat")}
{sess_col(width="230px", meta="goal active")}
<div class="main">
  <div class="topbar">
    <div style="display:flex;align-items:center;gap:10px"><span>{svg("chat")} Implement desktop UI preview…</span><span style="color:#6b6b6b">· saya-harness</span></div>
    <div class="goalbar">
      <span class="goal-pill">{svg("flag")}<span>Ship preview build</span><span class="gtime">12:34</span><span class="gbtn">{svg("pause")}</span><span class="gbtn">{svg("stop")}</span></span>
      <span class="badge green">CODE</span><span class="badge plain">auto</span>
    </div>
  </div>
  <div class="content">
    <div class="thread">
      <div class="msg-user">Find where session restore is implemented, then wire preview mode.</div>
      <div class="subagent">
        <div class="hd">{svg("agent")}<span style="font-weight:500">Explore subagent</span><span class="badge blue">subagent_explore</span><span class="badge amber">running</span><span style="flex:1"></span><span style="color:#6e6e6e">3 tool calls · 4.2k tok ›</span></div>
        <div class="sub">
          <div class="srow">{svg("terminal")}<span class="tname">grep</span><span>restore_session</span><span class="badge green">done</span></div>
          <div class="srow">{svg("terminal")}<span class="tname">read_file</span><span>session_store.rs</span><span class="badge green">done</span></div>
          <div class="srow">{svg("terminal")}<span class="tname">read_file</span><span>runtime_bridge.rs</span><span class="badge amber">running</span></div>
        </div>
      </div>
      <div class="md md-plain"><p>Dispatching a read-only subagent to locate the restore path while the main run continues wiring the preview flag…</p></div>
      <div class="toolcard">{svg("terminal")}<span class="tname">edit_file</span><span class="badge amber">running</span><span style="flex:1"></span><span style="color:#6e6e6e">src-tauri/src/commands.rs</span><span style="color:#4ade80;font-weight:600"> +42</span><span style="color:#f87171;font-weight:600"> -7</span></div>
      <div class="tpsline"><span class="live">● streaming</span><span>142 tok/s</span><span>12.4k tokens this run</span><span>elapsed 02:41</span></div>
    </div>
    <div class="composer-bar"><div class="inner">
      <div class="composer" style="width:100%"><div class="ta ph">Steer the run…</div>
        <div class="row"><span class="chip">{svg("plus")}</span>
        <div style="display:flex;gap:2px;align-items:center"><span class="chip"><span class="dot" style="background:#4ade80"></span>CODE</span><span class="chip">auto</span><span class="chip">claude-sonnet-4-5 Max</span><span class="chip" style="border:1px solid #2e2e2e">Steer ⌄</span>{ctxring(0.81)}<span class="send" title="Stop">{svg("stop")}</span></div></div>
      </div>
    </div></div>
  </div>
</div></div>'''

# ---------- 10 Usage & tokens ----------
_bars = [(12,5,1),(6,2,0),(20,9,1),(30,11,1),(8,4,0),(4,2,0),(15,7,0),(24,10,0),(19,9,0),(6,4,0),(32,13,1),(28,11,1),(9,5,0),(5,3,0),(44,17,1),(38,15,2),(62,24,2),(48,20,2),(42,16,2),(66,24,2),(52,20,2)]
_sc = 88.0 / max(i + o + c for i, o, c in _bars)
def _seg(h, c):
    return f'<i style="height:{round(h * _sc)}px;background:{c}"></i>' if h else ""
chart = "".join(
    f'<div class="bar" style="height:{round((i + o + c) * _sc)}px">'
    + _seg(c, "#6b6b6b") + _seg(o, "#8aa8d8") + _seg(i, "#d9a08e") + "</div>"
    for i, o, c in _bars)
usage = f'''<div class="app">{sidebar()}
<div class="main"><div class="content" style="align-items:center;justify-content:center">
  <div class="thread" style="filter:brightness(.5)"><div class="msg-user">Show me token usage for this month.</div></div>
  <div class="overlay" style="position:absolute">
    <div class="dialog" style="width:700px;height:560px">
      <div class="dnav">
        <a>{svg("gear")}General</a>
        <a>{svg("sliders")}Appearance</a>
        <a class="sel">{svg("cpu")}Usage</a>
        <a>{svg("cpu")}Model &amp; Profiles</a>
        <a>{svg("shield")}Permissions</a>
        <a>{svg("key")}MCP &amp; Credentials</a>
        <a>{svg("db")}Memory</a>
        <a>{svg("download")}Import &amp; export</a>
      </div>
      <div class="dbody" style="overflow:hidden">
        <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:8px">
          <span style="font-size:12px;color:#a3a3a3">Token usage across CLI, TUI and Desktop on this device</span>
          <span class="seg"><span>7d</span><span class="on">30d</span><span>90d</span></span>
        </div>
        <div class="usage-cards">
          <div class="ucard"><div class="uh">CLI</div><div class="ubig">12.4M <span class="us">tokens</span></div>
            <div class="urow"><span>Input</span><span>4.1M</span></div><div class="urow"><span>Output</span><span>8.3M</span></div><div class="urow"><span>Cache read</span><span>31.2M</span></div></div>
          <div class="ucard"><div class="uh">TUI</div><div class="ubig">58.9M <span class="us">tokens</span></div>
            <div class="urow"><span>Input</span><span>21.7M</span></div><div class="urow"><span>Output</span><span>37.2M</span></div><div class="urow"><span>Cache read</span><span>104M</span></div></div>
          <div class="ucard"><div class="uh">Desktop</div><div class="ubig">178.9M <span class="us">tokens</span></div>
            <div class="urow"><span>Input</span><span>152.3M</span></div><div class="urow"><span>Output</span><span>26.6M</span></div><div class="urow"><span>Cache read</span><span>7.9B</span></div></div>
        </div>
        <div class="card" style="padding:12px 14px">
          <div style="display:flex;justify-content:space-between;align-items:center"><span style="font-size:12.5px;color:#e8e8e8">Tokens per day</span><span style="font-size:11px;color:#6e6e6e;display:flex;gap:10px"><span><i style="display:inline-block;width:7px;height:7px;border-radius:2px;background:#d9a08e;margin-right:4px"></i>input</span><span><i style="display:inline-block;width:7px;height:7px;border-radius:2px;background:#8aa8d8;margin-right:4px"></i>output</span><span><i style="display:inline-block;width:7px;height:7px;border-radius:2px;background:#6b6b6b;margin-right:4px"></i>cache read</span></span></div>
          <div class="chart">{chart}
            <div class="tip"><div class="tt">Day 15<span>62k total</span></div><div class="tr"><i style="background:#d9a08e"></i>input 44k</div><div class="tr"><i style="background:#8aa8d8"></i>output 17k</div><div class="tr"><i style="background:#6b6b6b"></i>cache read 1k</div></div>
          </div>
          <div style="display:flex;justify-content:space-between;font-size:10px;color:#5e5e5e"><span>Oct 19</span><span>Nov 18</span></div>
        </div>
        <div class="tpsline" style="margin-top:10px"><span class="live">● live session</span><span>142 tok/s</span><span>12.4k tok this run</span><span>est. ctx fill 18%</span></div>
      </div>
    </div>
  </div>
</div></div></div>'''

# ---------- 11 Slash command palette ----------
cmdpal = f'''<div class="app">{sidebar()}
<div class="main"><div class="content">
  <div class="center-home" style="justify-content:flex-end;padding-bottom:180px">
    <div style="position:relative;width:620px">
      <div class="cmdpal">
        <div class="chead">Commands</div>
        <a class="sel"><span class="c">/model</span><span class="d">switch model profile</span></a>
        <a><span class="c">/mode</span><span class="d">change work mode — plan · build · code · work</span></a>
        <a><span class="c">/permission</span><span class="d">auto · bypass permissions · manual approval</span></a>
        <a><span class="c">/compact</span><span class="d">summarize context to free window space</span></a>
        <a><span class="c">/export</span><span class="d">export session to markdown handoff</span></a>
        <a><span class="c">/sessions</span><span class="d">browse and restore sessions</span></a>
        <a><span class="c">/memory</span><span class="d">view and edit workspace memory</span></a>
        <a><span class="c">/mcp</span><span class="d">manage MCP servers and credentials</span></a>
      </div>
      <div class="composer" style="width:100%">
        <div class="ta" style="color:#e8e8e8">/<span style="color:#6e6e6e">commands</span></div>
        <div class="row"><span class="chip">{svg("plus")}</span>
          <div style="display:flex;gap:2px;align-items:center"><span class="chip"><span class="dot" style="background:#4ade80"></span>CODE</span><span class="chip">auto</span><span class="chip">claude-sonnet-4-5 Max</span>{ctxring(0.68)}<span class="send off">{svg("send")}</span></div></div>
      </div>
    </div>
  </div>
</div></div></div>'''

# ---------- 12 Attachments + message preview ----------
attach_preview = f'''<div class="app">{sidebar()}
<div class="main"><div class="content">
  <div class="center-home" style="justify-content:flex-end;padding-bottom:150px">
    <div style="width:620px">
      <div class="composer" style="width:100%">
        <div class="attach-row">
          <span class="atch"><img src="{ICON}">app-icon.png<span style="color:#6e6e6e">22 KB</span><span class="x">{svg("x")}</span></span>
          <span class="atch">{svg("file")}AGENTS.md<span style="color:#6e6e6e">9 KB</span><span class="x">{svg("x")}</span></span>
          <span class="atch">{svg("file")}error.log<span style="color:#6e6e6e">4 KB</span><span class="x">{svg("x")}</span></span>
        </div>
        <div class="ta" style="color:#e8e8e8">Check why the icon renders **blurry** in the tray, see attached files.</div>
        <div class="msgpreview">
          <div class="ph">Message preview</div>
          <div style="font-size:12.5px;color:#c9c9c9;line-height:1.6">Check why the icon renders <b>blurry</b> in the tray, see attached files.<br><span style="color:#6e6e6e;font-size:11.5px">3 attachments · app-icon.png · AGENTS.md · error.log</span></div>
        </div>
        <div class="row" style="margin-top:10px"><span class="chip">{svg("plus")}</span>
          <div style="display:flex;gap:2px;align-items:center"><span class="chip"><span class="dot" style="background:#4ade80"></span>CODE</span><span class="chip">auto</span><span class="chip">claude-sonnet-4-5 Max</span>{ctxring(0.47)}<span class="send">{svg("send")}</span></div></div>
      </div>
    </div>
  </div>
</div></div></div>'''

# ---------- 13 Model & effort picker ----------
model_effort = f'''<div class="app">{sidebar()}
<div class="main"><div class="content">
  <div class="center-home" style="justify-content:flex-end;padding-bottom:190px">
    <div style="position:relative;width:640px">
      <div class="effpop" style="bottom:calc(100% + 14px);right:0">
        <div class="eh"><span>Effort <b>Ultracode</b></span><span style="color:#6e6e6e">?</span></div>
        <div class="elbl"><span>Faster</span><span>Smarter</span></div>
        <div class="efftrack">
          <div class="fillg"></div>
          <div class="gridfx"></div>
          <div class="stop" style="left:11%"></div><div class="stop" style="left:33%"></div><div class="stop" style="left:55%"></div><div class="stop" style="left:77%"></div>
          <div class="handle"></div>
        </div>
        <div style="font-size:11.5px;color:#8a8a8a;margin-top:10px;line-height:1.55">Highest effort, plus dynamic workflows for this session.</div>
        <div style="display:flex;justify-content:space-between;align-items:center;margin-top:10px;padding-top:10px;border-top:1px solid #262626">
          <div><div style="font-size:12px;color:#d9d9d9">Ultracode</div><div style="font-size:10.5px;color:#7a7a7a;max-width:190px;line-height:1.4">Standing dynamic-workflow orchestration for this session.</div></div>
          <span class="switch" style="background:#8a7bd8"></span>
        </div>
      </div>
      <div class="modelmenu" style="bottom:calc(100% + 14px);left:0">
        <div class="mh">Model profile</div>
        <a style="background:#262626">{svg("cpu")}anthropic-official<span class="m">sonnet-4-5 ✓</span></a>
        <a>{svg("cpu")}anthropic-official<span class="m">opus-4-6</span></a>
        <a>{svg("cpu")}deepseek<span class="m">v4.1-flash</span></a>
        <a>{svg("cpu")}qwen<span class="m">coder-max</span></a>
        <div class="sep" style="height:1px;background:#2a2a2a;margin:5px 4px"></div>
        <a style="color:#d9a08e">{svg("sliders")}Manage profiles…</a>
      </div>
      <div class="composer" style="width:100%">
        <div class="ta" style="color:#e8e8e8">Refactor the auth flow and add tests.</div>
        <div class="row">
          <div style="display:flex;gap:6px;align-items:center"><span class="iconchip">{svg("list")}</span><span class="chip">{svg("plus")}</span><span class="toolpill">{svg("globe")}Web search</span></div>
          <div style="display:flex;gap:2px;align-items:center">
            <span class="chip" style="border:1px solid #2e2e2e">Sonnet 4.5</span>
            <span class="chip" style="border:1px solid #4a4580;color:#c8b7f2">{svg("spark")}Effort: Ultracode ⌃</span>
            {ctxring(0.38)}<span class="send">{svg("send")}</span>
          </div>
        </div>
      </div>
      <div style="display:flex;gap:14px;justify-content:center;margin-top:10px;color:#5e5e5e;font-size:11px"><span>Drag the handle</span><span>release to snap</span><span>← → to step</span></div>
    </div>
  </div>
</div></div></div>'''

pages = {
    "01-home": home,
    "02-session": session,
    "03-approval": approval,
    "04-settings": settings,
    "05-user-menu": menu,
    "06-sessions": sessions_view,
    "07-attach-menu": attach_menu,
    "08-browser-preview": browser_preview,
    "09-subagent-goal": subagent_frame,
    "10-usage": usage,
    "11-command-palette": cmdpal,
    "12-attach-preview": attach_preview,
    "13-model-effort": model_effort,
}
for name, body in pages.items():
    with open(f"{OUT}/{name}.html", "w") as f:
        f.write(page(body))
    print(name, "written")

# ---------- 14 Settings — Language ----------
def _lang_tile(native, desc, sel=False):
    if sel:
        return f'''<div style="position:relative;flex:1;border:1px solid rgba(217,160,142,.55);background:rgba(217,160,142,.08);border-radius:10px;padding:12px 14px">
          <div style="position:absolute;top:10px;right:10px;color:#d9a08e">{svg("check")}</div>
          <div style="font-size:13.5px;font-weight:600;color:#e8e8e8">{native}</div>
          <div style="font-size:11.5px;color:#8a8a8a;margin-top:3px">{desc}</div>
        </div>'''
    return f'''<div style="position:relative;flex:1;border:1px solid #2e2e2e;background:#1c1c1c;border-radius:10px;padding:12px 14px">
      <div style="font-size:13.5px;font-weight:500;color:#d9d9d9">{native}</div>
      <div style="font-size:11.5px;color:#8a8a8a;margin-top:3px">{desc}</div>
    </div>'''

language = f'''<div class="app">{sidebar()}
<div class="main"><div class="content" style="align-items:center;justify-content:center">
  <div class="center-home" style="filter:brightness(.45)">
    <div class="greet"><img src="{ICON}">Good morning, ariakage.</div>
    <div class="composer" style="margin-top:34px">
      <div class="ta ph">How can I help you today?</div>
      <div class="row"><span class="chip">{svg("plus")}</span>{ctxring(0.12)}<span class="send off">{svg("send")}</span></div>
    </div>
  </div>
  <div class="overlay" style="position:absolute">
    <div style="width:460px;background:#181818;border:1px solid #2b2b2b;border-radius:14px;overflow:hidden;box-shadow:0 24px 60px rgba(0,0,0,.5)">
      <div style="display:flex;align-items:center;justify-content:space-between;padding:14px 16px;border-bottom:1px solid #232323">
        <span style="font-size:14px;font-weight:600;color:#e8e8e8">Choose your language</span>
        <span class="chip" style="padding:4px">{svg("x")}</span>
      </div>
      <div style="padding:16px">
        <div style="display:flex;gap:10px">
          {_lang_tile("English (United States)", "English (United States)", sel=True)}
          {_lang_tile("简体中文", "Simplified Chinese (China)")}
          {_lang_tile("繁體中文", "Traditional Chinese (Taiwan)")}
        </div>
        <div style="margin-top:14px;font-size:11.5px;color:#6e6e6e">Interface language for Soyo on this device. Restart may be required.</div>
      </div>
    </div>
  </div>
</div></div>'''

pages["14-language"] = language
with open(f"{OUT}/14-language.html", "w") as f:
    f.write(page(language))
print("14-language written")
