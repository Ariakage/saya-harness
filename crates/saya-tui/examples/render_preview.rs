//! Render the unified session view from a synthetic state using TestBackend.
#[cfg(debug_assertions)]
use ratatui::style::{Color, Modifier};
#[cfg(debug_assertions)]
use ratatui::{Terminal, backend::TestBackend, buffer::CellDiffOption};
#[cfg(debug_assertions)]
use saya_tui::{
    preview::{SCENES, fixture},
    view,
};
#[cfg(debug_assertions)]
use std::{error::Error, io::Write};
#[cfg(debug_assertions)]
use unicode_width::UnicodeWidthStr;

#[cfg(debug_assertions)]
fn color(value: Color, background: bool) -> String {
    match value {
        Color::Rgb(red, green, blue) => format!("#{red:02x}{green:02x}{blue:02x}"),
        Color::Reset if background => "#09090b".into(),
        Color::Reset => "#d4d4d8".into(),
        _ => if background { "#09090b" } else { "#d4d4d8" }.into(),
    }
}

#[cfg(debug_assertions)]
fn dimension(value: Option<&String>, default: u16, maximum: u16) -> Result<u16, Box<dyn Error>> {
    let value = value
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(default);
    if value == 0 || value > maximum {
        return Err(format!("preview dimension must be 1..={maximum}").into());
    }
    Ok(value)
}

#[cfg(debug_assertions)]
fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os()
        .skip(1)
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| "preview arguments must be UTF-8")
        })
        .collect::<Result<Vec<_>, _>>()?;
    if args.iter().any(|argument| argument == "--help") {
        writeln!(
            std::io::stdout().lock(),
            "render_preview [WIDTH HEIGHT] [SCENE] [--source] [--json]\nScenes: {}",
            SCENES.join(", ")
        )?;
        return Ok(());
    }
    let source = args.iter().any(|argument| argument == "--source");
    let json = args.iter().any(|argument| argument == "--json");
    args.retain(|argument| !matches!(argument.as_str(), "--source" | "--json"));
    if args.len() > 3 {
        return Err("usage: render_preview [WIDTH HEIGHT] [SCENE] [--source] [--json]".into());
    }
    let width = dimension(args.first(), 120, 240)?;
    let height = dimension(args.get(1), 30, 80)?;
    let scene = args.get(2).map(String::as_str).unwrap_or("chat");
    if !SCENES.contains(&scene) {
        return Err(format!("scene must be one of {}", SCENES.join(", ")).into());
    }
    let mut state = fixture(scene);
    if source {
        state.set_markdown_display(saya_tui::view::MarkdownDisplay::Source);
    }
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    terminal.draw(|frame| view::draw(frame, &mut state))?;
    let buffer = terminal.backend().buffer();
    if json {
        let mut cells = Vec::new();
        for y in 0..height {
            let mut x = 0;
            while x < width {
                let cell = &buffer[(x, y)];
                if cell.diff_option == CellDiffOption::Skip {
                    x += 1;
                    continue;
                }
                let columns = UnicodeWidthStr::width(cell.symbol())
                    .max(1)
                    .min(usize::from(width - x));
                cells.push(serde_json::json!({
                    "x": x, "y": y, "columns": columns, "text": cell.symbol(),
                    "foreground": color(cell.fg, false), "background": color(cell.bg, true),
                    "bold": cell.modifier.contains(Modifier::BOLD),
                    "italic": cell.modifier.contains(Modifier::ITALIC),
                    "underline": cell.modifier.contains(Modifier::UNDERLINED),
                    "strikethrough": cell.modifier.contains(Modifier::CROSSED_OUT),
                }));
                x += columns as u16;
            }
        }
        let value = serde_json::json!({"width":width, "height":height, "scene":scene, "source":source, "cells":cells});
        serde_json::to_writer(std::io::stdout().lock(), &value)?;
        return Ok(());
    }
    let mut output = String::new();
    for row in 0..height {
        let mut column = 0;
        let mut line = String::new();
        while column < width {
            let symbol = buffer[(column, row)].symbol();
            line.push_str(symbol);
            let cells = UnicodeWidthStr::width(symbol)
                .max(1)
                .min(usize::from(width - column));
            column += cells as u16;
        }
        output.push_str(line.trim_end());
        output.push('\n');
    }
    std::io::stdout().lock().write_all(output.as_bytes())?;
    Ok(())
}

#[cfg(not(debug_assertions))]
fn main() {
    eprintln!("render_preview requires a debug build");
    std::process::exit(2);
}
