#!/usr/bin/env python3
"""Generate Soyo Dark / Soyo Light HTML mocks of the Soyo desktop app.

Single-source generator reading CSS design tokens directly from
``desktop/src/theme/tokens.css`` and emitting standalone 1520x980 HTML frames.

Run from any directory:

    python3 docs/design/devin_theme_gen.py --theme dark /tmp/soyo_theme_dark
    python3 docs/design/devin_theme_gen.py --theme light /tmp/soyo_theme_light
"""

import argparse
import base64
import math
import os
import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
TOKENS_CSS_PATH = REPO_ROOT / "desktop" / "src" / "theme" / "tokens.css"
ICON_PATH = REPO_ROOT / "desktop" / "public" / "app-icon.png"
FONT_PATH = REPO_ROOT / "desktop" / "src" / "assets" / "fonts" / "SourceSerif4-Variable.woff2"


def load_b64(path: Path) -> str:
    return base64.b64encode(path.read_bytes()).decode("ascii")


def extract_token_rules(tokens_path: Path, font_b64: str) -> str:
    """Read @font-face and :root token blocks verbatim from tokens.css."""
    raw = tokens_path.read_text(encoding="utf-8")
    # Inline font woff2 for self-contained HTML frames while keeping exact family/weights.
    raw = raw.replace(
        'url("../assets/fonts/SourceSerif4-Variable.woff2")',
        f'url("data:font/woff2;base64,{font_b64}")',
    )
    # Strip Tailwind v4 @theme block so standalone HTML parses cleanly in Chrome/browsers.
    raw = re.sub(r"@theme\s*\{.*?\n\}\s*", "", raw, flags=re.DOTALL)
    return raw.strip()


BASE_CSS = """
*{box-sizing:border-box;margin:0;padding:0;border-color:var(--border)}
html,body{height:100%}
body{background:var(--surface-app);color:var(--text-primary);font-family:var(--font-sans);font-size:var(--text-ui);line-height:var(--leading-ui);-webkit-font-smoothing:antialiased;overflow:hidden}
.app{display:flex;flex-direction:column;height:100vh;width:100vw;background:var(--surface-app)}
.topbar{height:38px;flex:none;display:flex;align-items:center;border-bottom:1px solid var(--border-subtle);background:var(--surface-sidebar);color:var(--text-secondary);font-size:13px}
.topbar-left{width:248px;height:100%;flex:none;display:flex;align-items:center;justify-content:space-between;padding:0 10px 0 88px;border-right:1px solid var(--border-subtle)}
.topbar-right{flex:1;height:100%;min-width:0;display:flex;align-items:center;justify-content:space-between;padding:0 12px;gap:8px;background:var(--surface-app)}
.view-seg{display:inline-flex;height:26px;align-items:center;padding:2px;border-radius:var(--radius-md);border:1px solid var(--border);background:var(--surface-raised);gap:2px}
.view-seg span{width:22px;height:20px;border-radius:var(--radius-sm);display:flex;align-items:center;justify-content:center;color:var(--text-tertiary)}
.view-seg span.on{background:var(--surface-hover);color:var(--text-primary)}
.tb-btn{width:26px;height:26px;border-radius:var(--radius-sm);display:inline-flex;align-items:center;justify-content:center;color:var(--text-tertiary)}
.tb-btn:hover{background:var(--surface-hover);color:var(--text-primary)}
.workspace{display:flex;min-height:0;flex:1;overflow:hidden}
.sidebar{width:248px;flex:none;background:var(--surface-sidebar);border-right:1px solid var(--border-subtle);display:flex;flex-direction:column;padding:10px 8px}
.nav{display:flex;flex-direction:column;gap:2px}
.nav a{display:flex;align-items:center;gap:10px;padding:6px 10px;border-radius:var(--radius-md);color:var(--text-secondary);font-size:13px;line-height:20px}
.nav a:hover{background:var(--surface-hover);color:var(--text-primary)}
.nav a.primary{color:var(--accent-text);font-weight:500}
.nav a.active{background:var(--surface-hover);color:var(--text-primary);font-weight:500}
.nav a .k{margin-left:auto;font-size:11px;color:var(--text-tertiary)}
.section-label{margin:14px 10px 6px;font-size:11px;font-weight:600;letter-spacing:.04em;color:var(--text-tertiary);display:flex;align-items:center;justify-content:space-between}
.proj-hd{margin:8px 10px 4px;font-size:11px;color:var(--text-tertiary);display:flex;align-items:center;justify-content:space-between}
.proj-hd span.l{display:inline-flex;align-items:center;gap:6px}
.recent{display:flex;flex-direction:column;gap:2px}
.recent a{padding:6px 10px;border-radius:var(--radius-md);color:var(--text-secondary);font-size:13px}
.recent a .t{display:block;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.recent a .m{display:block;font-size:11px;color:var(--text-tertiary)}
.recent a.sel{background:var(--surface-hover);color:var(--text-primary)}
.user-chip{margin-top:auto;padding-top:8px;border-top:1px solid var(--border-subtle)}
.user-chip-inner{display:flex;align-items:center;gap:8px;padding:8px 10px;border-radius:var(--radius-md);color:var(--text-secondary);font-size:13px}
.user-chip img{width:22px;height:22px;border-radius:22%;object-fit:cover}
.main{flex:1;display:flex;flex-direction:column;min-width:0;background:var(--surface-app)}
.content{flex:1;overflow:hidden;display:flex;flex-direction:column;position:relative}
.center-home{flex:1;display:flex;flex-direction:column;align-items:center;justify-content:center;padding-bottom:48px}
.greet{display:flex;align-items:center;gap:12px;font-family:var(--font-display);font-size:var(--text-display);line-height:var(--leading-display);color:var(--text-primary);letter-spacing:-.01em}
.greet img{width:30px;height:30px;border-radius:22%;object-fit:cover}
.composer{width:620px;background:var(--surface-raised);border:1px solid var(--border);border-radius:var(--radius-xl);padding:12px 14px;box-shadow:var(--shadow-popover)}
.composer .ta{color:var(--text-primary);font-size:14px;line-height:22px;min-height:24px}
.composer .ta.ph{color:var(--text-tertiary)}
.composer .row{display:flex;align-items:center;justify-content:space-between;margin-top:10px;gap:8px}
.chip{display:inline-flex;align-items:center;gap:6px;color:var(--text-secondary);font-size:12px;line-height:18px;padding:4px 8px;border-radius:var(--radius-md);white-space:nowrap}
.capsule{display:inline-flex;height:28px;align-items:center;overflow:hidden;border-radius:var(--radius-md);border:1px solid var(--border);background:var(--surface-raised);white-space:nowrap}
.capsule.danger{border-color:var(--danger)}
.capsule .segbtn{display:inline-flex;height:100%;align-items:center;gap:6px;padding:0 8px;font-size:12px;color:var(--text-secondary);white-space:nowrap}
.capsule .segbtn.danger{background:var(--danger-surface);color:var(--danger);font-weight:500}
.capsule .segbtn.info{color:var(--info)}
.capsule .segbtn.ultra{background:var(--ultra-surface);color:var(--ultra-bright)}
.capsule .div{width:1px;height:14px;background:var(--border);flex:none}
.dot{width:6px;height:6px;border-radius:9999px;flex:none}
.send{width:28px;height:28px;border-radius:9999px;background:var(--accent);color:var(--on-accent);display:flex;align-items:center;justify-content:center;flex:none}
.send.off{background:var(--surface-raised);border:1px solid var(--border-strong);color:var(--text-primary);opacity:.4}
.ws-footer{width:592px;display:flex;align-items:center;justify-content:space-between;border:1px solid var(--border);border-top:none;background:var(--surface-raised);padding:8px 14px;border-radius:0 0 var(--radius-lg) var(--radius-lg);font-size:12px;color:var(--text-secondary)}
.ideas{margin-top:28px;display:flex;flex-direction:column;gap:4px;width:620px}
.ideas .lbl{font-size:11px;font-weight:500;color:var(--text-tertiary);padding:4px 12px}
.ideas a{display:flex;align-items:center;gap:12px;padding:8px 12px;border-radius:var(--radius-lg);border:1px solid transparent;color:var(--text-secondary);font-size:13px}
.ideas .ib{width:28px;height:28px;border-radius:var(--radius-md);background:var(--surface-raised);border:1px solid var(--border);display:flex;align-items:center;justify-content:center;color:var(--text-tertiary);flex:none}
.thread{flex:1;overflow:hidden;padding:18px 26px 8px;display:flex;flex-direction:column;gap:14px}
.msg-user{align-self:flex-end;max-width:640px;background:var(--surface-raised);border:1px solid var(--border);border-radius:var(--radius-lg);padding:10px 14px;font-size:14px;line-height:22px;color:var(--text-primary)}
.msg-sys{color:var(--text-tertiary);font-size:12px;padding:2px 14px}
.card{background:var(--surface-raised);border:1px solid var(--border);border-radius:var(--radius-lg);padding:12px 14px}
.md-plain{padding:2px 0}
.toolcard{display:flex;align-items:center;gap:8px;font-size:12px;color:var(--text-secondary);padding:6px 2px}
.thinking{display:flex;align-items:center;gap:8px;color:var(--text-tertiary);font-size:12px;padding:6px 2px}
.thinking-body{border-left:2px solid var(--border);margin:2px 2px 4px 7px;padding:2px 0 2px 14px;color:var(--text-secondary);font-size:13px;line-height:20px}
.toolcard .tname{font-family:var(--font-mono);color:var(--text-primary)}
.badge{display:inline-flex;align-items:center;gap:5px;font-size:12px;padding:2px 9px;border-radius:9999px;font-weight:500;background:var(--surface-raised);color:var(--text-secondary);border:1px solid var(--border);white-space:nowrap}
.badge.success{color:var(--success);border-color:var(--success);background:var(--success-surface)}
.badge.info{color:var(--info);border-color:var(--info);background:var(--info-surface)}
.badge.warning{color:var(--warning);border-color:var(--warning);background:var(--warning-surface)}
.badge.danger{color:var(--danger);border-color:var(--danger);background:var(--danger-surface)}
.badge.work{color:var(--mode-work)}
.badge.code{color:var(--mode-code)}
.badge.plan{color:var(--mode-plan)}
.badge.build{color:var(--mode-build)}
.md h3{font-size:16px;line-height:24px;margin:8px 0 6px;color:var(--text-primary)}
.md a{color:var(--info);text-decoration:none}
.md p{color:var(--text-primary);line-height:22px;font-size:14px;margin:6px 0}
.md ul{color:var(--text-primary);line-height:22px;font-size:14px;padding-left:20px;margin:6px 0}
.md code{font-family:var(--font-mono);font-size:12px;background:var(--surface-code-inline);border:1px solid var(--border-subtle);padding:1px 5px;border-radius:var(--radius-sm);color:var(--text-primary)}
.codeblock{background:var(--surface-code);border:1px solid var(--border);border-radius:var(--radius-lg);padding:10px 12px;font-family:var(--font-mono);font-size:12px;line-height:20px;color:var(--text-primary);margin:8px 0}
.codeblock .k{color:var(--danger)}.codeblock .s{color:var(--info)}.codeblock .f{color:var(--mode-plan)}.codeblock .c{color:var(--text-tertiary)}
.composer-bar{flex:none;padding:8px 24px 14px;display:flex;justify-content:center}
.composer-bar .inner{width:100%;max-width:840px}
.kbd-hint{color:var(--text-tertiary);font-size:11px;line-height:16px;display:flex;gap:16px;justify-content:center;margin-top:8px}
.panel{width:308px;flex:none;border-left:1px solid var(--border-subtle);background:var(--surface-sidebar);overflow:hidden;padding:14px;display:flex;flex-direction:column;gap:16px}
.panel h4{font-size:12px;color:var(--text-secondary);font-weight:500;display:flex;align-items:center;justify-content:space-between}
.panel .prow{display:flex;justify-content:space-between;font-size:12px;color:var(--text-primary);padding:4px 0}
.panel .prow .k{color:var(--text-tertiary)}
.ctxbar{display:flex;height:6px;border-radius:var(--radius-sm);overflow:hidden;margin:8px 0 10px;background:var(--surface-hover)}
.ctxbar div{height:100%}
.legend{display:flex;flex-direction:column;gap:5px;font-size:12px}
.legend .li{display:flex;align-items:center;justify-content:space-between;color:var(--text-secondary)}
.legend .li .l{display:flex;align-items:center;gap:7px}
.legend .sw{width:8px;height:8px;border-radius:var(--radius-sm)}
.approval{background:var(--warning-surface);border:1px solid var(--warning);border-radius:var(--radius-lg);padding:14px}
.approval .ttl{display:flex;align-items:center;justify-content:space-between;color:var(--text-primary);font-size:13px;font-weight:600}
.approval .kv{display:grid;grid-template-columns:auto 1fr;gap:4px 12px;font-size:12px;margin:10px 0;background:var(--surface-raised);border:1px solid var(--border);border-radius:var(--radius-md);padding:10px 12px}
.approval .kv .k{color:var(--text-tertiary)}
.approval .kv .v{color:var(--text-primary);font-family:var(--font-mono);font-size:12px}
.approval .cmd{background:var(--surface-code);border:1px solid var(--border);border-radius:var(--radius-md);font-family:var(--font-mono);font-size:12px;color:var(--text-primary);padding:8px 10px;margin:8px 0}
.btnrow{display:flex;gap:8px;justify-content:flex-end;margin-top:10px}
.btn{padding:6px 14px;border-radius:var(--radius-md);font-size:12px;font-weight:500;border:1px solid var(--border-strong);background:var(--surface-raised);color:var(--text-primary)}
.btn.primary{background:var(--accent);border-color:var(--accent);color:var(--on-accent)}
.overlay{position:absolute;inset:0;background:var(--scrim);display:flex;align-items:center;justify-content:center}
.dialog{width:760px;background:var(--surface-overlay);border:1px solid var(--border);border-radius:var(--radius-xl);overflow:hidden;box-shadow:var(--shadow-dialog);display:flex}
.dialog .dnav{width:200px;flex:none;border-right:1px solid var(--border-subtle);background:var(--surface-sidebar);padding:12px 10px;display:flex;flex-direction:column;gap:2px}
.dialog .dnav a{display:flex;align-items:center;gap:8px;padding:7px 10px;border-radius:var(--radius-md);color:var(--text-secondary);font-size:13px}
.dialog .dnav a.sel{background:var(--surface-hover);color:var(--text-primary);font-weight:500}
.dialog .dnav .gsep{height:1px;background:var(--border-subtle);margin:6px 4px}
.dialog .dbody{flex:1;padding:20px;overflow:hidden;background:var(--surface-overlay)}
.dialog .dbody h3{font-size:16px;line-height:24px;color:var(--text-primary);margin-bottom:14px;font-weight:600}
.field{margin-bottom:16px}
.field .fl{font-size:12px;font-weight:500;color:var(--text-secondary);margin-bottom:6px}
.field .fv{background:var(--surface-raised);border:1px solid var(--border-strong);border-radius:var(--radius-md);padding:7px 10px;font-size:13px;color:var(--text-primary);display:flex;justify-content:space-between;align-items:center}
.seg{display:inline-flex;background:var(--surface-raised);border:1px solid var(--border);border-radius:var(--radius-md);padding:2px;gap:2px}
.seg span{padding:4px 12px;border-radius:var(--radius-sm);font-size:12px;color:var(--text-secondary)}
.seg span.on{background:var(--surface-hover);color:var(--text-primary);font-weight:500}
.switch{width:34px;height:20px;border-radius:var(--radius-sm);background:var(--accent);position:relative}
.switch::after{content:"";position:absolute;width:15px;height:15px;border-radius:var(--radius-sm);background:var(--on-accent);top:2.5px;right:2.5px}
.switch.off{background:var(--border-strong)}
.switch.off::after{left:2.5px;right:auto}
.menu{position:absolute;left:10px;bottom:56px;width:232px;background:var(--surface-overlay);border:1px solid var(--border);border-radius:var(--radius-lg);padding:6px;box-shadow:var(--shadow-popover);z-index:20}
.menu a{display:flex;align-items:center;gap:9px;padding:7px 10px;border-radius:var(--radius-md);color:var(--text-primary);font-size:13px}
.menu .sep{height:1px;background:var(--border-subtle);margin:5px 4px}
.menu .hd{padding:6px 10px 4px;font-size:12px;color:var(--text-tertiary)}
.menu .k{margin-left:auto;color:var(--text-tertiary);font-size:11px}
.attach-menu{position:absolute;width:220px;background:var(--surface-overlay);border:1px solid var(--border);border-radius:var(--radius-lg);padding:6px;box-shadow:var(--shadow-popover);z-index:20}
.attach-menu a{display:flex;align-items:center;gap:9px;padding:7px 10px;border-radius:var(--radius-md);color:var(--text-primary);font-size:13px}
.attach-menu .k{margin-left:auto;color:var(--text-tertiary);font-size:11px}
.hoverrow{display:flex;align-items:center;gap:12px;justify-content:flex-end;color:var(--text-tertiary);font-size:11px;padding:4px 4px 0}
.hovertag{font-size:11px;color:var(--text-tertiary);border:1px dashed var(--border-strong);border-radius:var(--radius-sm);padding:1px 6px;margin-left:4px}
.browser{flex:1;display:flex;flex-direction:column;border-left:1px solid var(--border-subtle);background:var(--surface-sidebar);min-width:0}
.bchrome{height:40px;flex:none;display:flex;align-items:center;gap:8px;padding:0 12px;border-bottom:1px solid var(--border-subtle);color:var(--text-secondary)}
.bchrome .navbtn{color:var(--text-tertiary);width:24px;height:24px;border-radius:var(--radius-sm);display:flex;align-items:center;justify-content:center}
.bchrome .url{flex:1;display:flex;align-items:center;gap:7px;background:var(--surface-raised);border:1px solid var(--border);border-radius:var(--radius-md);padding:4px 10px;font-size:12px;color:var(--text-secondary);justify-content:center}
.bview{flex:1;overflow:hidden;position:relative;background:var(--surface-app)}
.mini-app{display:flex;height:100%;font-size:11px}
.mini-sb{width:118px;flex:none;background:var(--surface-sidebar);border-right:1px solid var(--border-subtle);padding:8px;display:flex;flex-direction:column;gap:2px}
.mini-sb .mi{padding:4px 7px;border-radius:var(--radius-sm);color:var(--text-secondary);font-size:11px}
.mini-sb .mi.on{background:var(--surface-raised);color:var(--text-primary)}
.mini-main{flex:1;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:14px;padding:20px}
.mini-greet{display:flex;align-items:center;gap:8px;font-family:var(--font-display);font-size:16px;color:var(--text-primary)}
.mini-greet img{width:18px;height:18px;border-radius:22%}
.mini-comp{width:80%;max-width:320px;background:var(--surface-raised);border:1px solid var(--border);border-radius:var(--radius-lg);padding:10px;font-size:11px}
.mini-comp .row{display:flex;justify-content:space-between;align-items:center;margin-top:10px}
.mini-chip{font-size:11px;color:var(--text-secondary);background:var(--surface-hover);border-radius:var(--radius-sm);padding:2px 6px}
.goal-pill{display:inline-flex;align-items:center;gap:8px;background:var(--warning-surface);border:1px solid var(--warning);border-radius:var(--radius-md);padding:3px 10px;color:var(--warning);font-size:12px}
.goal-pill .gbtn{width:16px;height:16px;border-radius:var(--radius-sm);background:var(--surface-raised);border:1px solid var(--warning);display:flex;align-items:center;justify-content:center;color:var(--warning)}
.subagent{border:1px solid var(--border);border-left:2px solid var(--accent);border-radius:var(--radius-lg);padding:10px 12px;background:var(--surface-raised)}
.subagent .hd{display:flex;align-items:center;gap:9px;font-size:13px;color:var(--text-primary)}
.subagent .sub{margin:8px 0 0 15px;border-left:1px solid var(--border);padding-left:12px;display:flex;flex-direction:column;gap:5px}
.subagent .srow{display:flex;align-items:center;gap:8px;font-size:12px;color:var(--text-secondary)}
.tpsline{display:flex;align-items:center;gap:14px;color:var(--text-tertiary);font-size:11px;padding:2px 4px;font-variant-numeric:tabular-nums}
.tpsline .live{display:inline-flex;align-items:center;gap:6px;color:var(--success)}
.cmdpal{position:absolute;left:0;right:0;bottom:calc(100% + 8px);background:var(--surface-overlay);border:1px solid var(--border);border-radius:var(--radius-lg);padding:6px;box-shadow:var(--shadow-popover)}
.cmdpal .chead{font-size:11px;color:var(--text-tertiary);padding:4px 9px}
.cmdpal a{display:flex;align-items:baseline;gap:10px;padding:6px 9px;border-radius:var(--radius-sm);font-size:13px}
.cmdpal a .c{font-family:var(--font-mono);color:var(--info);min-width:110px}
.cmdpal a .d{color:var(--text-secondary);font-size:12px}
.cmdpal a.sel{background:var(--surface-hover)}
.attach-row{display:flex;gap:8px;margin-bottom:9px}
.atch{display:flex;align-items:center;gap:8px;background:var(--surface-hover);border:1px solid var(--border);border-radius:var(--radius-md);padding:6px 9px;font-size:12px;color:var(--text-primary)}
.atch img{width:26px;height:26px;border-radius:var(--radius-sm);object-fit:cover}
.msgpreview{border:1px dashed var(--border-strong);border-radius:var(--radius-lg);padding:10px 12px;margin-top:10px}
.msgpreview .ph{font-size:11px;color:var(--text-tertiary);letter-spacing:.04em;text-transform:uppercase;margin-bottom:6px}
.usage-cards{display:flex;gap:10px;margin:10px 0}
.ucard{flex:1;background:var(--surface-raised);border:1px solid var(--border);border-radius:var(--radius-lg);padding:12px}
.ucard .uh{font-size:12px;color:var(--text-secondary);margin-bottom:6px}
.ucard .ubig{font-size:16px;color:var(--text-primary);font-weight:600}
.ucard .us{font-size:11px;color:var(--text-tertiary);font-weight:400}
.ucard .urow{display:flex;justify-content:space-between;font-size:11px;color:var(--text-tertiary);padding:2px 0}
.diff{border:1px solid var(--border);border-radius:var(--radius-lg);overflow:hidden;background:var(--surface-code);margin-top:4px}
.diff .dh{display:flex;align-items:center;gap:10px;padding:7px 11px;border-bottom:1px solid var(--border-subtle);font-size:12px;color:var(--text-secondary);font-family:var(--font-mono)}
.diff .dh .plus{color:var(--success);font-weight:600}
.diff .dh .minus{color:var(--danger);font-weight:600}
.diff .dl{display:flex;align-items:center;font-family:var(--font-mono);font-size:12px;line-height:20px;color:var(--text-secondary);white-space:pre}
.diff .dl .ln{width:36px;flex:none;color:var(--text-tertiary);text-align:right;padding-right:8px;font-size:11px}
.diff .dl .sg{width:16px;flex:none;text-align:center;color:var(--text-tertiary)}
.diff .dl.add{background:var(--success-surface);color:var(--diff-add-text)}
.diff .dl.add .sg{color:var(--success)}
.diff .dl.del{background:var(--danger-surface);color:var(--diff-del-text)}
.diff .dl.del .sg{color:var(--danger)}
.diff .dl.hunk{background:var(--info-surface);color:var(--info);font-size:11px}
.diff .more{padding:5px 11px;border-top:1px solid var(--border-subtle);font-size:11px;color:var(--text-tertiary);text-align:center}
.chart{display:flex;align-items:flex-end;gap:4px;height:110px;padding-top:8px;position:relative}
.tip{position:absolute;left:69%;bottom:96px;transform:translateX(-50%);background:var(--surface-overlay);border:1px solid var(--border-strong);border-radius:var(--radius-md);padding:6px 9px;font-size:11px;color:var(--text-secondary);box-shadow:var(--shadow-popover);white-space:nowrap;z-index:5}
.tip .tt{font-weight:500;color:var(--text-primary);margin-bottom:3px}
.tip .tt span{color:var(--text-tertiary);font-weight:400;margin-left:5px}
.tip .tr{display:flex;align-items:center;gap:6px;padding:1px 0}
.tip i{display:inline-block;width:6px;height:6px;border-radius:var(--radius-sm)}
.chart .bar{flex:1;min-width:4px;display:flex;flex-direction:column}
.chart .bar>i{display:block;width:100%}
.chart .bar>i:first-child{border-radius:var(--radius-sm) var(--radius-sm) 0 0}
.chart .bar>i:last-child{border-radius:0 0 var(--radius-sm) var(--radius-sm)}
.toolpill{display:inline-flex;align-items:center;gap:6px;border:1px solid var(--info);border-radius:var(--radius-md);padding:3px 10px;font-size:12px;color:var(--info);background:var(--info-surface);white-space:nowrap}
.effpop{position:absolute;width:276px;background:var(--surface-overlay);border:1px solid var(--border);border-radius:var(--radius-lg);padding:14px;box-shadow:var(--shadow-popover);z-index:20}
.effpop .eh{display:flex;align-items:center;justify-content:space-between;font-size:13px;color:var(--text-secondary)}
.effpop .eh b{color:var(--ultra-bright);font-weight:600}
.effpop .elbl{display:flex;justify-content:space-between;font-size:11px;color:var(--text-tertiary);margin:12px 0 4px}
.efftrack{position:relative;height:24px;border-radius:var(--radius-sm);background:var(--surface-hover)}
.efftrack .fillg{position:absolute;inset:0;border-radius:var(--radius-sm);background:linear-gradient(90deg,var(--border) 0%,var(--ultra-surface) 60%,var(--ultra-surface) 100%)}
.efftrack .gridfx{position:absolute;inset:0;border-radius:var(--radius-sm);background-image:radial-gradient(circle at 1.6px 1.6px,var(--ultra-bright) 1.15px,transparent 1.3px);background-size:5px 5px;-webkit-mask-image:linear-gradient(90deg,transparent 0%,var(--text-primary) 45%);mask-image:linear-gradient(90deg,transparent 0%,var(--text-primary) 45%)}
.efftrack .stop{position:absolute;top:10.5px;width:3px;height:3px;border-radius:9999px;background:var(--text-tertiary);margin-left:-1.5px}
.efftrack .handle{position:absolute;top:0;right:0;width:20px;height:24px;border-radius:var(--radius-sm);background:var(--surface-raised);border:1px solid var(--border-strong);box-shadow:var(--shadow-popover)}
.modelmenu{position:absolute;width:228px;background:var(--surface-overlay);border:1px solid var(--border);border-radius:var(--radius-lg);padding:6px;box-shadow:var(--shadow-popover);z-index:20}
.modelmenu .mh{font-size:11px;color:var(--text-tertiary);padding:4px 9px 6px}
.modelmenu a{display:flex;align-items:center;gap:9px;padding:7px 9px;border-radius:var(--radius-md);color:var(--text-primary);font-size:13px}
.modelmenu a .m{color:var(--text-tertiary);font-size:11px;margin-left:auto}
.sqloader{position:relative;display:inline-block;flex:none;vertical-align:middle}
.sqloader span{position:absolute;top:0;left:0;width:39.1304%;height:39.1304%;border-radius:27.778%}
.sqloader span.a{background:var(--loader-secondary);transform:translate(0%,0%)}
.sqloader span.b{background:var(--loader-primary);transform:translate(155.5556%,0%)}
.sqloader span.c{background:var(--loader-secondary);transform:translate(0%,155.5556%)}
"""


def svg(name: str) -> str:
    icons = {
        "plus": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M12 5v14M5 12h14"/></svg>',
        "search": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><circle cx="11" cy="11" r="7"/><path d="m21 21-4.3-4.3"/></svg>',
        "chat": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/></svg>',
        "box": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z"/></svg>',
        "clock": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 3"/></svg>',
        "sliders": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3M1 14h6M9 8h6M17 16h6"/></svg>',
        "db": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><ellipse cx="12" cy="5" rx="8" ry="3"/><path d="M4 5v14c0 1.7 3.6 3 8 3s8-1.3 8-3V5"/><path d="M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3"/></svg>',
        "key": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="m21 2-2 2m-7.6 7.6a5.5 5.5 0 1 1-7.8 7.8 5.5 5.5 0 0 1 7.8-7.8zm0 0L19 4m3 3-3-3"/></svg>',
        "download": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3"/></svg>',
        "gear": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><circle cx="12" cy="12" r="3"/><path d="M12 1v4m0 14v4M4.2 4.2l2.8 2.8m10 10 2.8 2.8M1 12h4m14 0h4M4.2 19.8l2.8-2.8m10-10 2.8-2.8"/></svg>',
        "panelLeft": '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><rect x="3" y="3" width="18" height="18" rx="2"/><path d="M9 3v18"/></svg>',
        "panelRight": '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><rect x="3" y="3" width="18" height="18" rx="2"/><path d="M15 3v18"/></svg>',
        "code2": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="m18 16 4-4-4-4M6 8l-4 4 4 4m8.5-12-5 16"/></svg>',
        "shield": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/></svg>',
        "shieldAlert": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><path d="M12 8v4m0 4h.01"/></svg>',
        "hand": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M18 11V6a2 2 0 0 0-4 0v4M14 10V4a2 2 0 0 0-4 0v6M10 10.5V6a2 2 0 0 0-4 0v8l-1.3-1.3a2 2 0 0 0-2.8 2.8L7 21h9a4 4 0 0 0 4-4v-6a2 2 0 0 0-2-2z"/></svg>',
        "dots": '<svg width="15" height="15" viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.5"/><circle cx="12" cy="12" r="1.5"/><circle cx="19" cy="12" r="1.5"/></svg>',
        "help": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><circle cx="12" cy="12" r="9"/><path d="M9.1 9a3 3 0 0 1 5.8 1c0 2-3 3-3 3m.1 4h.01"/></svg>',
        "send": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1"><path d="M12 19V5m-7 7 7-7 7 7"/></svg>',
        "attach": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="m21.4 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l8.57-8.57A4 4 0 1 1 18 8.84l-8.59 8.57a2 2 0 0 1-2.83-2.83l8.49-8.48"/></svg>',
        "paste": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><rect x="8" y="2" width="8" height="4" rx="1"/><path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2"/></svg>',
        "at": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><circle cx="12" cy="12" r="4"/><path d="M16 8v5a3 3 0 0 0 6 0v-1a10 10 0 1 0-4 8"/></svg>',
        "terminal": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="m4 17 6-6-6-6M12 19h8"/></svg>',
        "check": '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="var(--success)" stroke-width="2.2"><path d="M20 6 9 17l-5-5"/></svg>',
        "circle": '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="var(--text-tertiary)" stroke-width="1.75"><circle cx="12" cy="12" r="9"/></svg>',
        "half": '<svg width="12" height="12" viewBox="0 0 24 24" fill="var(--warning)"><circle cx="12" cy="12" r="5"/></svg>',
        "x": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M18 6 6 18M6 6l12 12"/></svg>',
        "folder": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/></svg>',
        "globe": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a15 15 0 0 1 0 18 15 15 0 0 1 0-18z"/></svg>',
        "cpu": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><rect x="4" y="4" width="16" height="16" rx="2"/><rect x="9" y="9" width="6" height="6"/><path d="M9 1v3m6-3v3M9 20v3m6-3v3M1 9h3m-3 6h3M20 9h3m-3 6h3"/></svg>',
        "warn": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="var(--warning)" stroke-width="1.75"><path d="m12 3 10 18H2z"/><path d="M12 10v4m0 3h.01"/></svg>',
        "file": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><path d="M14 2v6h6"/></svg>',
        "share": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M7 17 17 7M8 7h9v9"/></svg>',
        "retry": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M21 12a9 9 0 1 1-3-6.7L21 8"/><path d="M21 3v5h-5"/></svg>',
        "edit": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M17 3a2.8 2.8 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5z"/></svg>',
        "copy": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10"/></svg>',
        "back": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M19 12H5m7 7-7-7 7-7"/></svg>',
        "fwd": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M5 12h14m-7-7 7 7-7 7"/></svg>',
        "reload": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M21 12a9 9 0 1 1-3-6.7L21 8"/><path d="M21 3v5h-5"/></svg>',
        "lock": '<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="var(--text-tertiary)" stroke-width="1.75"><rect x="4" y="11" width="16" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/></svg>',
        "external": '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6M15 3h6v6M10 14 21 3"/></svg>',
        "pause": '<svg width="10" height="10" viewBox="0 0 24 24" fill="currentColor"><rect x="5" y="4" width="5" height="16" rx="1.2"/><rect x="14" y="4" width="5" height="16" rx="1.2"/></svg>',
        "stop": '<svg width="9" height="9" viewBox="0 0 24 24" fill="currentColor"><rect x="5" y="5" width="14" height="14" rx="2"/></svg>',
        "agent": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><circle cx="12" cy="8" r="4"/><path d="M4 21v-1a7 7 0 0 1 14 0v1"/></svg>',
        "flag": '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M4 15s1-1 4-1 5 2 8 2 4-1 4-1V3s-1 1-4 1-5-2-8-2-4 1-4 1z"/><path d="M4 22v-7"/></svg>',
        "minimize": '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M4 14h6v6M20 10h-6V4M14 10l7-7M3 21l7-7"/></svg>',
        "gauge": '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="m12 14 4-4"/><path d="M3.34 19a10 10 0 1 1 17.32 0"/></svg>',
        "chevRight": '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="m9 18 6-6-6-6"/></svg>',
        "chevDown": '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="m6 9 6 6 6-6"/></svg>',
        "palette": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><circle cx="13.5" cy="6.5" r=".5" fill="currentColor"/><circle cx="17.5" cy="10.5" r=".5" fill="currentColor"/><circle cx="8.5" cy="7.5" r=".5" fill="currentColor"/><circle cx="6.5" cy="12.5" r=".5" fill="currentColor"/><path d="M12 2C6.5 2 2 6.5 2 12s4.5 10 10 10c.9 0 1.6-.7 1.6-1.6 0-.4-.2-.8-.4-1.1-.3-.3-.4-.7-.4-1.1 0-.9.7-1.6 1.6-1.6H16c3.3 0 6-2.7 6-6 0-5.2-4.5-8.6-10-8.6z"/></svg>',
        "activity": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M22 12h-4l-3 9L9 3l-3 9H2"/></svg>',
        "sunmoon": '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><circle cx="12" cy="12" r="4"/><path d="M12 3v1m0 16v1m9-9h-1M4 12H3m15.4 6.4-.7-.7M6.3 6.3l-.7-.7m12.8 0-.7.7M6.3 17.7l-.7.7"/></svg>',
    }
    return icons.get(name, "")


def squares_loader(size: int = 12) -> str:
    return (
        f'<span class="sqloader" style="width:{size}px;height:{size}px">'
        '<span class="a"></span><span class="b"></span><span class="c"></span>'
        "</span>"
    )


def ctxring(pct: float = 0.5) -> str:
    r, c = 6.0, 2 * math.pi * 6.0
    tone = (
        "var(--accent)"
        if pct < 0.75
        else ("var(--warning)" if pct < 0.92 else "var(--danger)")
    )
    dash = max(1.0, c * pct)
    return (
        '<span style="display:inline-flex;align-items:center;margin:0 2px" title="Context window">'
        '<svg width="16" height="16" viewBox="0 0 16 16">'
        f'<circle cx="8" cy="8" r="{r}" fill="none" stroke="var(--border)" stroke-width="2.2"/>'
        f'<circle cx="8" cy="8" r="{r}" fill="none" stroke="{tone}" stroke-width="2.2" '
        f'stroke-linecap="round" stroke-dasharray="{dash:.1f} {c:.1f}" transform="rotate(-90 8 8)"/>'
        "</svg></span>"
    )


def topbar(
    title: str = "Implement desktop UI preview with Vite HMR",
    project: str = "soyo-harness",
    mode: str = "CODE",
    perm: str = "auto",
    goal: bool = False,
    icon_b64: str = "",
) -> str:
    del icon_b64
    mode_var = f"var(--mode-{mode.lower()})"
    if perm == "bypass":
        perm_html = f'<span class="badge danger">{svg("shieldAlert")}<span>Bypass permissions</span></span>'
    elif perm == "manual":
        perm_html = f'<span class="badge info">{svg("hand")}<span>Manual approval</span></span>'
    else:
        perm_html = '<span class="badge">Auto</span>'
    goal_html = (
        f'<span class="goal-pill">{svg("flag")}<span>Ship preview build</span>'
        f'<span style="opacity:.8">12:34</span><span class="gbtn">{svg("pause")}</span>'
        f'<span class="gbtn">{svg("stop")}</span></span>'
        if goal
        else ""
    )
    return f'''<header class="topbar">
  <div class="topbar-left">
    <span class="tb-btn">{svg("panelLeft")}</span>
    <div class="view-seg"><span class="on">{svg("chat")}</span><span>{svg("code2")}</span></div>
  </div>
  <div class="topbar-right">
    <div style="display:flex;align-items:center;gap:6px;min-width:0">
      <span style="color:var(--text-primary);font-weight:500">{title}</span>
      <span style="color:var(--text-tertiary)">· {project}</span>
      {svg("chevDown")}
    </div>
    <div style="display:flex;align-items:center;gap:6px">
      {goal_html}
      <span class="badge"><span class="dot" style="background:{mode_var}"></span><span style="color:{mode_var}">{mode}</span></span>
      {perm_html}
      <span class="tb-btn">{svg("panelRight")}</span>
      <span class="tb-btn">{svg("globe")}</span>
      <span class="tb-btn">{svg("gear")}</span>
      <span class="tb-btn">{svg("dots")}</span>
    </div>
  </div>
</header>'''


def sidebar(icon: str, active: str = "new", selected_session: str = "Implement desktop UI preview with Vite HMR") -> str:
    nav_items = [
        ("new", "plus", "New session", "⌘N"),
        ("search", "search", "Search", "⌘K"),
        ("sessions", "clock", "Sessions", ""),
        ("memory", "db", "Memory", ""),
        ("mcp", "key", "MCP & credentials", ""),
        ("export", "download", "Export", ""),
        ("customize", "palette", "Customize", ""),
    ]
    nav_html = "".join(
        f'<a class="{"primary" if k == "new" else ("active" if k == active else "")}">'
        f'{svg(ic)}<span>{lbl}</span>'
        f'{f"<span class=k>{kbd}</span>" if kbd else ""}</a>'
        for k, ic, lbl, kbd in nav_items
    )
    return f'''<aside class="sidebar">
  <div class="nav">{nav_html}</div>
  <div class="section-label">RECENTS <span style="color:var(--text-tertiary)">···</span></div>
  <div class="proj-hd"><span class="l">{svg("folder")} soyo-harness</span><span>2</span></div>
  <div class="recent">
    <a class="{"sel" if selected_session.startswith("Implement") else ""}"><span class="t">Implement desktop UI preview with Vite HMR</span><span class="m">code · completed</span></a>
    <a><span class="t">Markdown, Math &amp; Mermaid showcase</span><span class="m">plan · completed</span></a>
  </div>
  <div class="proj-hd"><span class="l">{svg("folder")} AkaMemo</span><span>1</span></div>
  <div class="recent">
    <a class="{"sel" if selected_session.startswith("项目") else ""}"><span class="t">项目 AGENTS.md 与文档规范 (fork)</span><span class="m">code · completed</span></a>
  </div>
  <div class="proj-hd"><span class="l">{svg("folder")} ai-gateway</span><span>1</span></div>
  <div class="recent">
    <a><span class="t">导出会话到 HANDOFF 文档</span><span class="m">work · completed</span></a>
  </div>
  <div style="padding:8px 10px 0;font-size:12px;color:var(--text-tertiary)">View all</div>
  <div class="user-chip">
    <div class="user-chip-inner">
      <img src="{icon}" alt="User">
      <div style="min-width:0;flex:1">
        <div style="color:var(--text-primary);font-weight:500">ariakage</div>
        <div style="font-size:11px;color:var(--text-tertiary)">anthropic-official</div>
      </div>
    </div>
  </div>
</aside>'''


def mode_perm_capsule(mode: str = "CODE", perm: str = "auto", show_mode: bool = True) -> str:
    mode_var = f"var(--mode-{mode.lower()})"
    cap_cls = "capsule danger" if perm == "bypass" else "capsule"
    mode_part = (
        f'<span class="segbtn"><span class="dot" style="background:{mode_var}"></span><span>{mode}</span></span>'
        '<span class="div"></span>'
        if show_mode
        else ""
    )
    if perm == "bypass":
        perm_part = f'<span class="segbtn danger">{svg("shieldAlert")}<span>Bypass permissions</span></span>'
    elif perm == "manual":
        perm_part = f'<span class="segbtn info">{svg("hand")}<span>Manual approval</span></span>'
    else:
        perm_part = '<span class="segbtn">Auto</span>'
    return f'<div class="{cap_cls}">{mode_part}{perm_part}</div>'


def model_effort_capsule(model: str = "claude-sonnet-4-5", effort: str = "Medium", ultra: bool = False, home: bool = False) -> str:
    if home:
        return (
            '<div class="capsule">'
            f'<span class="segbtn">{model} · {effort}</span>'
            "</div>"
        )
    eff_cls = "segbtn ultra" if ultra else "segbtn"
    eff_label = "Effort: Ultracode" if ultra else f"Effort: {effort}"
    return (
        '<div class="capsule">'
        f'<span class="segbtn">{model}</span>'
        '<span class="div"></span>'
        f'<span class="{eff_cls}">{svg("gauge")}<span>{eff_label}</span>{svg("chevDown")}</span>'
        "</div>"
    )


def composer_block(
    placeholder: str = "Type a message or / for commands…",
    mode: str = "CODE",
    perm: str = "auto",
    effort: str = "Medium",
    ultra: bool = False,
    pct: float = 0.5,
    running: bool = False,
    web_chip: bool = False,
    paused: bool = False,
    home: bool = False,
) -> str:
    left_extras = (
        f'<span class="toolpill">{svg("globe")}Web search</span>'
        if web_chip
        else ""
    )
    paused_chip = (
        '<span class="badge warning">paused · approval required</span>'
        if paused
        else ""
    )
    loader_chip = squares_loader(12) if running else ""
    send_cls = "send" if (running or not home) else "send off"
    send_ic = svg("stop") if running else svg("send")
    return f'''<div class="composer" style="width:100%">
  <div class="ta ph">{placeholder}</div>
  <div class="row">
    <div style="display:flex;align-items:center;gap:6px">
      <span class="tb-btn" style="border:1px solid var(--border);border-radius:var(--radius-md)">{svg("plus")}</span>
      {left_extras}
      {paused_chip}
      {loader_chip}
    </div>
    <div style="display:flex;align-items:center;gap:6px">
      {"" if home and perm == "auto" else mode_perm_capsule(mode=mode, perm=perm, show_mode=not home)}
      {model_effort_capsule(effort=effort, ultra=ultra, home=home)}
      {ctxring(pct)}
      <span class="{send_cls}">{send_ic}</span>
    </div>
  </div>
</div>'''


def build_frames(icon: str) -> dict[str, str]:
    home = f'''<div class="app">
  {topbar(title="Home", mode="WORK")}
  <div class="workspace">
    {sidebar(icon)}
    <div class="main"><div class="content">
      <div class="center-home">
        <div class="greet"><img src="{icon}" alt="Soyo">Morning, ariakage.</div>
        <div style="width:620px;margin-top:32px">{composer_block(placeholder="How can I help you today?", pct=0.12, home=True)}</div>
        <div class="ws-footer"><span style="display:inline-flex;align-items:center;gap:6px">{svg("folder")} soyo-harness</span><span style="font-family:var(--font-mono);color:var(--text-tertiary)">/workspace/soyo-harness</span></div>
        <div class="ideas">
          <div class="lbl">Ideas for you</div>
          <a><span class="ib">{svg("clock")}</span>Summarize today's sessions into a handoff note</a>
          <a><span class="ib">{svg("box")}</span>Audit MCP servers and credential scopes</a>
          <a><span class="ib">{svg("sliders")}</span>Customize Soyo for this workspace</a>
        </div>
      </div>
    </div></div>
  </div>
</div>'''

    session = f'''<div class="app">
  {topbar(mode="CODE")}
  <div class="workspace">
    {sidebar(icon)}
    <div class="main"><div class="content">
      <div class="thread">
        <div style="display:flex;flex-direction:column">
          <div class="msg-user">Check the runtime boundary for soyo-desktop.</div>
          <div class="hoverrow"><span>Oct 28</span>{svg("share")}{svg("retry")}{svg("edit")}{svg("copy")}<span class="hovertag">on hover</span></div>
        </div>
        <div class="thinking">{svg("chevRight")}<span>Thought for 8 s</span><span class="badge">auto-collapsed</span></div>
        <div class="toolcard">{svg("terminal")}<span class="tname">read_file</span><span class="badge success">done</span><span style="flex:1"></span><span style="color:var(--text-tertiary)">crates/soyo-app/src/lib.rs · 90 lines</span></div>
        <div class="md md-plain">
          <p>In-process linking directly invokes <code>soyo_app::spawn_build_runtime</code> over <code>Channel&lt;DesktopStreamEnvelope&gt;</code>:</p>
          <ul><li>Blocking SQLite reads stay isolated in <code>offload_boundary</code>.</li><li>No remote assets or keyring plugins are loaded.</li></ul>
          <div class="codeblock"><span class="k">pub async fn</span> <span class="f">cancel_run</span>(&amp;<span class="k">self</span>) -&gt; <span class="k">bool</span> {{<br>&nbsp;&nbsp;&nbsp;&nbsp;<span class="k">self</span>.runtime.cancel_run()<br>}}</div>
        </div>
        <div class="toolcard">{svg("edit")}<span class="tname">edit_file</span><span class="badge success">done</span><span style="flex:1"></span><span style="color:var(--text-tertiary)">desktop/src/App.tsx</span><span style="color:var(--success);font-weight:600">+128</span><span style="color:var(--danger);font-weight:600">-12</span></div>
        <div class="diff">
          <div class="dh"><span>desktop/src/App.tsx</span><span class="plus">+128</span><span class="minus">-12</span><span style="flex:1"></span><span>2 hunks</span></div>
          <div class="dl hunk"><span class="ln"></span><span class="sg"></span><span>@@ -855,7 +855,10 @@ export function App()</span></div>
          <div class="dl"><span class="ln">856</span><span class="sg"> </span><span>const [copiedId, setCopiedId] = useState&lt;string | null&gt;(null)</span></div>
          <div class="dl add"><span class="ln"></span><span class="sg">+</span><span>const [editDraft, setEditDraft] = useState&lt;Draft | null&gt;(null)</span></div>
          <div class="dl del"><span class="ln">912</span><span class="sg">-</span><span>&lt;Pencil className="h-3 w-3 opacity-50" /&gt;</span></div>
          <div class="more">122 unchanged lines hidden · expand</div>
        </div>
        <div class="thinking">{squares_loader(12)}<span>Thinking</span></div>
        <div class="toolcard">{svg("terminal")}<span class="tname">exec</span>{squares_loader(12)}<span class="badge warning">running</span><span style="flex:1"></span><span style="color:var(--text-tertiary)">$ git status -s</span></div>
      </div>
      <div class="composer-bar"><div class="inner">
        {composer_block(mode="CODE", running=True, web_chip=True, pct=0.62)}
        <div class="kbd-hint"><span>Enter send</span><span>Shift+Enter newline</span><span>Empty Tab cycle mode</span><span>Esc cancel</span></div>
      </div></div>
    </div></div>
    <div class="panel">
      <div>
        <h4>Runtime <span class="dot" style="background:var(--success)"></span></h4>
        <div class="prow"><span class="k">Profile</span><span>anthropic-official</span></div>
        <div class="prow"><span class="k">Model</span><span>claude-sonnet-4-5</span></div>
        <div class="prow"><span class="k">Sandbox</span><span>seatbelt</span></div>
        <div class="prow"><span class="k">Workspace</span><span style="font-family:var(--font-mono)">~/soyo-harness</span></div>
      </div>
      <div>
        <h4>Context window <span>187.4k</span></h4>
        <div class="ctxbar"><div style="width:55%;background:var(--accent)"></div><div style="width:12%;background:var(--warning)"></div><div style="width:10%;background:var(--success)"></div><div style="width:5%;background:var(--info)"></div></div>
        <div class="legend">
          <div class="li"><span class="l"><span class="sw" style="background:var(--accent)"></span>Messages</span><span>103.1k · 55%</span></div>
          <div class="li"><span class="l"><span class="sw" style="background:var(--warning)"></span>Skills</span><span>22.5k · 12%</span></div>
          <div class="li"><span class="l"><span class="sw" style="background:var(--success)"></span>MCP tools</span><span>18.7k · 10%</span></div>
          <div class="li"><span class="l"><span class="sw" style="background:var(--info)"></span>System prompt</span><span>9.4k · 5%</span></div>
          <div class="li"><span class="l"><span class="sw" style="background:var(--surface-hover)"></span>Free space</span><span>33.7k · 18%</span></div>
        </div>
      </div>
      <div>
        <h4>Tasks <span>1/3</span></h4>
        <div style="display:flex;flex-direction:column;gap:6px;font-size:12px;margin-top:6px">
          <div style="display:flex;gap:8px;color:var(--text-tertiary);text-decoration:line-through">{svg("check")}<span>Wire Channel streaming</span></div>
          <div style="display:flex;gap:8px;color:var(--warning)">{svg("half")}<span>Verify Streamdown + Mermaid fallback</span></div>
          <div style="display:flex;gap:8px;color:var(--text-secondary)">{svg("circle")}<span>Approval dialog round-trip</span></div>
        </div>
      </div>
      <div>
        <h4>Outputs <span>2</span></h4>
        <div style="display:flex;flex-direction:column;gap:4px;font-size:12px;margin-top:6px;color:var(--text-secondary)">
          <div style="display:flex;gap:8px">{svg("file")}<span>crates/soyo-app/src/lib.rs</span></div>
          <div style="display:flex;gap:8px">{svg("file")}<span>desktop/src/App.tsx</span></div>
        </div>
      </div>
    </div>
  </div>
</div>'''

    approval = f'''<div class="app">
  {topbar(mode="CODE", perm="manual")}
  <div class="workspace">
    {sidebar(icon)}
    <div class="main"><div class="content">
      <div class="thread">
        <div class="msg-user">Run the workspace verification tests.</div>
        <div class="approval">
          <div class="ttl"><span style="display:flex;align-items:center;gap:8px">{svg("warn")} Approval required</span><span class="badge code">CODE</span></div>
          <div class="kv">
            <span class="k">Tool</span><span class="v">exec</span>
            <span class="k">Action</span><span class="v" style="color:var(--info)">shell.exec</span>
            <span class="k">Call</span><span class="v">call_exec_01</span>
            <span class="k">Workspace</span><span class="v">~/soyo-harness</span>
          </div>
          <div style="font-size:13px;color:var(--text-secondary)">Run workspace verification tests with cargo</div>
          <div class="cmd">$ cargo test -p soyo-core --locked</div>
          <div style="display:flex;align-items:center;justify-content:space-between">
            <span style="font-size:11px;color:var(--text-tertiary)">Press Y to allow or N to deny</span>
            <div class="btnrow"><span class="btn">Deny (N)</span><span class="btn primary">Allow (Y)</span></div>
          </div>
        </div>
        <div class="msg-sys">Paused · waiting for your decision — the run resumes after you reply.</div>
      </div>
      <div class="composer-bar"><div class="inner">
        {composer_block(placeholder="Steer the run, or reply to approval above…", mode="CODE", perm="manual", paused=True, pct=0.74)}
      </div></div>
    </div></div>
  </div>
</div>'''

    settings = f'''<div class="app">
  {topbar(mode="CODE")}
  <div class="workspace">
    {sidebar(icon, active="customize")}
    <div class="main"><div class="content">
      <div class="overlay">
        <div class="dialog" style="height:560px">
          <div class="dnav">
            <a class="sel">{svg("gear")}General</a>
            <a>{svg("palette")}Appearance</a>
            <a>{svg("cpu")}Model &amp; profiles</a>
            <a>{svg("shield")}Permissions</a>
            <div class="gsep"></div>
            <a>{svg("box")}Modules</a>
            <a>{svg("key")}MCP &amp; credentials</a>
            <a>{svg("db")}Memory</a>
            <div class="gsep"></div>
            <a>{svg("activity")}Usage</a>
            <a>{svg("download")}Import &amp; export</a>
            <div style="margin-top:auto;padding:8px 10px 0;font-size:11px;color:var(--text-tertiary)">Soyo v0.3.0</div>
          </div>
          <div class="dbody">
            <h3>Runtime &amp; modules</h3>
            <div class="field"><div class="fl">Workspace directory</div><div class="fv"><span style="font-family:var(--font-mono);font-size:12px">~/soyo-harness</span><span class="btn" style="padding:3px 10px">Apply</span></div></div>
            <div style="display:flex;gap:10px">
              <div class="field" style="flex:1"><div class="fl">Model profile</div><div class="fv"><span>anthropic-official</span>{svg("chevDown")}</div></div>
              <div class="field" style="flex:1"><div class="fl">Thinking effort</div><div class="fv"><span>medium</span>{svg("chevDown")}</div></div>
              <div class="field" style="flex:1"><div class="fl">Sandbox backend</div><div class="fv"><span>seatbelt</span>{svg("chevDown")}</div></div>
            </div>
            <div class="field"><div class="fl" style="display:flex;justify-content:space-between"><span>Auto-compact</span><span class="badge success">On</span></div><div class="fv"><span style="font-family:var(--font-mono);font-size:12px">160000</span><span class="btn" style="padding:3px 10px">Set trigger</span></div></div>
            <div class="field"><div class="fl">Default work mode</div><div class="seg"><span>PLAN</span><span>BUILD</span><span class="on">CODE</span><span>WORK</span></div></div>
            <div class="field" style="border-top:1px solid var(--border-subtle);padding-top:14px">
              <div style="display:flex;justify-content:space-between;align-items:center">
                <div><div class="fl" style="margin-bottom:2px">About Soyo · v0.3.0</div><div style="font-size:12px;color:var(--text-tertiary)">Helper daemon connected · PID 42100 · 1 client</div></div>
                <span class="btn">Check for updates</span>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div></div>
  </div>
</div>'''

    menu = f'''<div class="app">
  {topbar(title="Home", mode="WORK")}
  <div class="workspace">
    {sidebar(icon)}
    <div class="main"><div class="content">
      <div class="center-home">
        <div class="greet"><img src="{icon}" alt="Soyo">You're here!</div>
        <div style="width:620px;margin-top:32px">{composer_block(placeholder="How can I help you today?", pct=0.34, home=True)}</div>
      </div>
    </div></div>
    <div class="menu">
      <div class="hd">ariakage · anthropic-official</div>
      <a>{svg("sunmoon")}Theme<span class="k">System</span></a>
      <a>{svg("globe")}Language<span class="k">en-US</span></a>
      <div class="sep"></div>
      <a>{svg("gear")}Settings<span class="k">⌘,</span></a>
      <a>{svg("activity")}Usage overview</a>
      <a>{svg("help")}About Soyo</a>
    </div>
  </div>
</div>'''

    sessions_view = f'''<div class="app">
  {topbar(title="项目 AGENTS.md 与文档规范 (fork)", project="AkaMemo", mode="CODE", perm="bypass")}
  <div class="workspace">
    {sidebar(icon, active="sessions", selected_session="项目 AGENTS.md 与文档规范 (fork)")}
    <div class="main"><div class="content">
      <div class="thread">
        <div class="md md-plain" style="max-width:760px">
          <h3 style="margin-top:0">合并结果</h3>
          <div style="display:grid;grid-template-columns:110px 1fr;gap:6px 14px;font-size:13px">
            <span style="color:var(--text-tertiary)">PR</span><span style="color:var(--info)">Ariakage/AkaMemo#1</span>
            <span style="color:var(--text-tertiary)">状态</span><span>MERGED</span>
            <span style="color:var(--text-tertiary)">变更规模</span><span>39 个文件、5920 行新增</span>
          </div>
          <h3>当前仓库状态</h3>
          <ul><li>GitHub main 已包含全部文档与工程配置</li><li>本地 main 与 origin/main 同步，工作区干净</li></ul>
        </div>
      </div>
      <div class="composer-bar"><div class="inner">
        {composer_block(placeholder="Type / for commands", mode="CODE", perm="bypass", pct=0.47)}
      </div></div>
    </div></div>
  </div>
</div>'''

    attach_menu = f'''<div class="app">
  {topbar(title="Home", mode="WORK")}
  <div class="workspace">
    {sidebar(icon)}
    <div class="main"><div class="content">
      <div class="center-home">
        <div class="greet"><img src="{icon}" alt="Soyo">Good evening, ariakage.</div>
        <div style="position:relative;margin-top:32px;width:620px">
          {composer_block(placeholder="Type / for skills", pct=0.25, home=True)}
          <div class="attach-menu" style="left:8px;top:116px">
            <a>{svg("attach")}Add files or photos<span class="k">⌘U</span></a>
            <a>{svg("paste")}Paste path or media</a>
            <a>{svg("at")}Insert @ mention</a>
            <a>{svg("terminal")}Insert / command</a>
            <a>{svg("minimize")}Compact context</a>
            <div style="height:1px;background:var(--border-subtle);margin:4px"></div>
            <a>{svg("globe")}Web search<span class="k">Off</span></a>
          </div>
        </div>
      </div>
    </div></div>
  </div>
</div>'''

    browser_preview = f'''<div class="app">
  {topbar(mode="CODE")}
  <div class="workspace">
    {sidebar(icon)}
    <div class="main"><div class="content" style="flex-direction:row">
      <div style="flex:1.15;display:flex;flex-direction:column;min-width:0">
        <div class="thread">
          <div class="msg-user">Spin up the Vite preview and check the new theme.</div>
          <div class="toolcard">{svg("terminal")}<span class="tname">exec</span><span class="badge success">done</span><span style="flex:1"></span><span style="color:var(--text-tertiary)">$ pnpm --dir desktop run dev</span></div>
          <div class="md md-plain"><p>Preview is live on the right — both Soyo Dark and Soyo Light render cleanly under shared tokens.</p></div>
        </div>
        <div class="composer-bar" style="padding:10px 18px 14px"><div class="inner">
          {composer_block(placeholder="Attach a screenshot of the preview", mode="CODE", pct=0.55)}
        </div></div>
      </div>
      <div class="browser">
        <div class="bchrome">
          <span class="navbtn">{svg("back")}</span><span class="navbtn">{svg("fwd")}</span><span class="navbtn">{svg("reload")}</span>
          <span class="url">{svg("lock")} localhost:1420</span>
          <span class="navbtn">{svg("external")}</span><span class="navbtn">{svg("x")}</span>
        </div>
        <div class="bview">
          <div class="mini-app">
            <div class="mini-sb"><div class="mi on">+ New session</div><div class="mi">Search</div><div class="mi">Sessions</div><div class="mi">Memory</div><div class="mi">Customize</div></div>
            <div class="mini-main">
              <div class="mini-greet"><img src="{icon}" alt="Soyo">Good morning, ariakage.</div>
              <div class="mini-comp"><div style="color:var(--text-tertiary)">How can I help you today?</div>
                <div class="row"><span class="mini-chip">+</span><span style="display:flex;gap:4px;align-items:center"><span class="mini-chip">claude-sonnet-4-5</span><span class="send" style="width:20px;height:20px">{svg("send")}</span></span></div>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div></div>
  </div>
</div>'''

    subagent = f'''<div class="app">
  {topbar(mode="CODE", goal=True)}
  <div class="workspace">
    {sidebar(icon)}
    <div class="main"><div class="content">
      <div class="thread">
        <div class="msg-user">Find where session restore is implemented, then wire preview mode.</div>
        <div class="subagent">
          <div class="hd">{svg("agent")}<span style="font-weight:500">Explore subagent</span><span class="badge info">subagent_explore</span><span class="badge warning">running</span>{squares_loader(12)}<span style="flex:1"></span><span style="color:var(--text-tertiary);font-size:12px">3 tool calls · 4.2k tok</span></div>
          <div class="sub">
            <div class="srow">{svg("terminal")}<span class="tname">grep</span><span>restore_session</span><span class="badge success">done</span></div>
            <div class="srow">{svg("terminal")}<span class="tname">read_file</span><span>session_store.rs</span><span class="badge success">done</span></div>
            <div class="srow">{svg("terminal")}<span class="tname">read_file</span><span>host_service.rs</span><span class="badge warning">running</span></div>
          </div>
        </div>
        <div class="md md-plain"><p>Dispatching a read-only subagent to locate the restore path while the main run continues wiring the preview flag…</p></div>
        <div class="toolcard">{svg("edit")}<span class="tname">edit_file</span>{squares_loader(12)}<span class="badge warning">running</span><span style="flex:1"></span><span style="color:var(--text-tertiary)">src-tauri/src/commands.rs</span><span style="color:var(--success);font-weight:600">+42</span><span style="color:var(--danger);font-weight:600">-7</span></div>
        <div class="tpsline"><span class="live">{squares_loader(12)} streaming</span><span>142 tok/s</span><span>12.4k tokens this run</span><span>elapsed 02:41</span></div>
      </div>
      <div class="composer-bar"><div class="inner">
        {composer_block(placeholder="Steer the run…", mode="CODE", running=True, pct=0.81)}
      </div></div>
    </div></div>
  </div>
</div>'''

    bars = [(12,5,1),(6,2,0),(20,9,1),(30,11,1),(8,4,0),(4,2,0),(15,7,0),(24,10,0),(19,9,0),(6,4,0),(32,13,1),(28,11,1),(9,5,0),(5,3,0)]
    sc = 88.0 / max(i + o + c for i, o, c in bars)
    def seg(h: int, color: str) -> str:
        return f'<i style="height:{round(h * sc)}px;background:{color}"></i>' if h else ""
    chart = "".join(
        f'<div class="bar" style="height:{round((i + o + c) * sc)}px">{seg(c, "var(--text-tertiary)")}{seg(o, "var(--info)")}{seg(i, "var(--accent)")}</div>'
        for i, o, c in bars
    )
    usage = f'''<div class="app">
  {topbar(mode="CODE")}
  <div class="workspace">
    {sidebar(icon)}
    <div class="main"><div class="content">
      <div class="overlay">
        <div class="dialog" style="height:560px">
          <div class="dnav">
            <a>{svg("gear")}General</a>
            <a>{svg("palette")}Appearance</a>
            <a>{svg("cpu")}Model &amp; profiles</a>
            <a>{svg("shield")}Permissions</a>
            <div class="gsep"></div>
            <a>{svg("box")}Modules</a>
            <a>{svg("key")}MCP &amp; credentials</a>
            <a>{svg("db")}Memory</a>
            <div class="gsep"></div>
            <a class="sel">{svg("activity")}Usage</a>
            <a>{svg("download")}Import &amp; export</a>
            <div style="margin-top:auto;padding:8px 10px 0;font-size:11px;color:var(--text-tertiary)">Soyo v0.3.0</div>
          </div>
          <div class="dbody">
            <h3>Usage &amp; tokens</h3>
            <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:8px">
              <span style="font-size:12px;color:var(--text-secondary)">Token usage across sessions on this device</span>
              <span class="seg"><span>7d</span><span class="on">30d</span><span>90d</span></span>
            </div>
            <div class="usage-cards">
              <div class="ucard"><div class="uh">Execution</div><div class="ubig">~20.3k <span class="us">tokens</span></div><div class="urow"><span>Input</span><span>~15.2k</span></div><div class="urow"><span>Output</span><span>~5.1k</span></div><div class="urow"><span>Cache read</span><span>~4.2k</span></div></div>
              <div class="ucard"><div class="uh">Review</div><div class="ubig">2.7k <span class="us">tokens</span></div><div class="urow"><span>Input</span><span>2.1k</span></div><div class="urow"><span>Output</span><span>640</span></div><div class="urow"><span>Cache read</span><span>400</span></div></div>
              <div class="ucard"><div class="uh">Compaction</div><div class="ubig">1.5k <span class="us">tokens</span></div><div class="urow"><span>Input</span><span>1.1k</span></div><div class="urow"><span>Output</span><span>400</span></div><div class="urow"><span>Cache read</span><span>200</span></div></div>
            </div>
            <div class="card">
              <div style="display:flex;justify-content:space-between;align-items:center"><span style="font-size:13px">Tokens per day</span><span style="font-size:11px;color:var(--text-tertiary);display:flex;gap:10px"><span>input</span><span>output</span><span>cache read</span></span></div>
              <div class="chart">{chart}</div>
              <div style="display:flex;justify-content:space-between;font-size:11px;color:var(--text-tertiary)"><span>Window: 30d</span><span>Today</span></div>
            </div>
            <div class="tpsline" style="margin-top:10px"><span class="live">{squares_loader(10)} live session</span><span>1.4k in · 380 out</span></div>
          </div>
        </div>
      </div>
    </div></div>
  </div>
</div>'''

    cmdpal = f'''<div class="app">
  {topbar(title="Home", mode="WORK")}
  <div class="workspace">
    {sidebar(icon)}
    <div class="main"><div class="content">
      <div class="center-home" style="justify-content:flex-end;padding-bottom:140px">
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
          {composer_block(placeholder="/", mode="WORK", pct=0.68)}
        </div>
      </div>
    </div></div>
  </div>
</div>'''

    attach_preview = f'''<div class="app">
  {topbar(title="Home", mode="CODE")}
  <div class="workspace">
    {sidebar(icon)}
    <div class="main"><div class="content">
      <div class="center-home" style="justify-content:flex-end;padding-bottom:120px">
        <div style="width:620px">
          <div class="composer" style="width:100%">
            <div class="attach-row">
              <span class="atch"><img src="{icon}" alt="app-icon.png">app-icon.png<span style="color:var(--text-tertiary)">22 KB</span>{svg("x")}</span>
              <span class="atch">{svg("file")}AGENTS.md<span style="color:var(--text-tertiary)">9 KB</span>{svg("x")}</span>
            </div>
            <div class="ta">Check why the icon renders **blurry** in the tray, see attached files.</div>
            <div class="msgpreview"><div class="ph">Message preview</div><div style="font-size:13px;color:var(--text-secondary)">Check why the icon renders <b>blurry</b> in the tray, see attached files.</div></div>
            <div class="row"><span class="tb-btn" style="border:1px solid var(--border);border-radius:var(--radius-md)">{svg("plus")}</span>
              <div style="display:flex;align-items:center;gap:6px">{mode_perm_capsule(mode="CODE")}{model_effort_capsule(effort="Max")}{ctxring(0.47)}<span class="send">{svg("send")}</span></div>
            </div>
          </div>
        </div>
      </div>
    </div></div>
  </div>
</div>'''

    model_effort = f'''<div class="app">
  {topbar(title="Home", mode="CODE")}
  <div class="workspace">
    {sidebar(icon)}
    <div class="main"><div class="content">
      <div class="center-home" style="justify-content:flex-end;padding-bottom:150px">
        <div style="position:relative;width:620px">
          <div class="effpop" style="bottom:calc(100% + 12px);right:0">
            <div class="eh"><span>Effort <b>Ultracode</b></span><span style="color:var(--text-tertiary)">?</span></div>
            <div class="elbl"><span>Faster</span><span>Smarter</span></div>
            <div class="efftrack"><div class="fillg"></div><div class="gridfx"></div><div class="stop" style="left:11%"></div><div class="stop" style="left:33%"></div><div class="stop" style="left:55%"></div><div class="stop" style="left:77%"></div><div class="handle"></div></div>
            <div style="font-size:12px;color:var(--text-secondary);margin-top:10px">Highest effort, plus dynamic workflows for this session.</div>
            <div style="display:flex;justify-content:space-between;align-items:center;margin-top:10px;padding-top:10px;border-top:1px solid var(--border-subtle)">
              <div><div style="font-size:12px;font-weight:500">Ultracode</div><div style="font-size:11px;color:var(--text-tertiary)">Standing dynamic-workflow orchestration.</div></div>
              <span class="switch" style="background:var(--ultra)"></span>
            </div>
          </div>
          <div class="modelmenu" style="bottom:calc(100% + 12px);left:0">
            <div class="mh">Model profile</div>
            <a style="background:var(--surface-hover)">{svg("cpu")}anthropic-official<span class="m">sonnet-4-5</span></a>
            <a>{svg("cpu")}anthropic-official<span class="m">opus-4-6</span></a>
            <a>{svg("cpu")}deepseek<span class="m">v4.1-flash</span></a>
            <div style="height:1px;background:var(--border-subtle);margin:4px"></div>
            <a style="color:var(--accent-text)">{svg("sliders")}Manage profiles…</a>
          </div>
          {composer_block(placeholder="Refactor the auth flow and add tests.", mode="CODE", effort="Ultracode", ultra=True, web_chip=True, pct=0.38)}
        </div>
      </div>
    </div></div>
  </div>
</div>'''

    appearance = f'''<div class="app">
  {topbar(mode="WORK")}
  <div class="workspace">
    {sidebar(icon, active="customize")}
    <div class="main"><div class="content">
      <div class="overlay">
        <div class="dialog" style="height:560px">
          <div class="dnav">
            <a>{svg("gear")}General</a>
            <a class="sel">{svg("palette")}Appearance</a>
            <a>{svg("cpu")}Model &amp; profiles</a>
            <a>{svg("shield")}Permissions</a>
            <div class="gsep"></div>
            <a>{svg("box")}Modules</a>
            <a>{svg("key")}MCP &amp; credentials</a>
            <a>{svg("db")}Memory</a>
            <div class="gsep"></div>
            <a>{svg("activity")}Usage</a>
            <a>{svg("download")}Import &amp; export</a>
            <div style="margin-top:auto;padding:8px 10px 0;font-size:11px;color:var(--text-tertiary)">Soyo v0.3.0</div>
          </div>
          <div class="dbody">
            <h3>Appearance &amp; language</h3>
            <div class="field"><div class="fl">Theme</div><div class="seg"><span>System</span><span>Light</span><span class="on">Dark</span></div></div>
            <div class="field"><div class="fl">Language</div><div class="fv"><span>English (United States) — English (United States)</span>{svg("chevDown")}</div></div>
            <div class="field"><div class="fl">Message font size</div><div class="seg"><span>13px Compact</span><span class="on">14px Default</span><span>16px Large</span></div></div>
            <div class="field"><div class="fl">Conversation font</div><div class="seg"><span class="on">Default sans</span><span>Serif (Source Serif 4)</span><span>System UI</span></div></div>
            <div class="field"><div style="display:flex;justify-content:space-between;align-items:center"><div><div class="fl" style="margin-bottom:2px">Reduce motion</div><div style="font-size:12px;color:var(--text-tertiary)">Disable transitions, loader animation, and Effort WebGL grid motion.</div></div><span class="switch off"></span></div></div>
          </div>
        </div>
      </div>
    </div></div>
  </div>
</div>'''

    return {
        "01-home": home,
        "02-session": session,
        "03-approval": approval,
        "04-settings": settings,
        "05-user-menu": menu,
        "06-sessions": sessions_view,
        "07-attach-menu": attach_menu,
        "08-browser-preview": browser_preview,
        "09-subagent-goal": subagent,
        "10-usage": usage,
        "11-command-palette": cmdpal,
        "12-attach-preview": attach_preview,
        "13-model-effort": model_effort,
        "14-appearance": appearance,
    }


def render_page(token_css: str, body: str, theme: str) -> str:
    return (
        f'<!DOCTYPE html><html data-theme="{theme}"><head><meta charset="utf-8">'
        f"<style>{token_css}\n{BASE_CSS}</style></head><body>{body}</body></html>"
    )


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Generate Soyo Desktop theme HTML frames from desktop/src/theme/tokens.css."
    )
    parser.add_argument(
        "out_dir",
        nargs="?",
        default="/tmp/soyo_devin_theme",
        help="Output directory for generated HTML frames",
    )
    parser.add_argument(
        "--theme",
        choices=("dark", "light"),
        default="dark",
        help="Target Soyo theme mode (default: dark)",
    )
    args = parser.parse_args()

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    icon_uri = f"data:image/png;base64,{load_b64(ICON_PATH)}"
    font_b64 = load_b64(FONT_PATH)
    token_css = extract_token_rules(TOKENS_CSS_PATH, font_b64)
    frames = build_frames(icon_uri)

    for name, body in frames.items():
        (out_dir / f"{name}.html").write_text(
            render_page(token_css, body, args.theme), encoding="utf-8"
        )
    print(f"Generated {len(frames)} {args.theme} HTML frames -> {out_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
