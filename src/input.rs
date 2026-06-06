use anyhow::{bail, Result};
use std::io::{self, IsTerminal, Read, Write};

const DEFAULT_COMPOSER_WIDTH: usize = 120;
const COMMAND_COLUMN_WIDTH: usize = 18;
const MAX_MENU_ROWS: usize = 5;
const FOOTER_RIGHT_MARGIN: usize = 2;
const COMPOSER_SAFE_AREA_LINES: usize = 2;
const ACTIVE_COLOR: &str = "\x1b[1m\x1b[38;2;0;168;204m";

#[derive(Debug, Clone, Copy)]
pub struct Suggestion<'a> {
    pub value: &'a str,
    pub detail: &'a str,
}

pub struct ComposerChrome {
    pub left_hint: String,
    pub right_hint: String,
    pub picker_hint: String,
    pub active_commands: Vec<String>,
    pub help_rows: Vec<HelpRow>,
    pub status_rows: Vec<StatusRow>,
    pub prompt_suggestions: Vec<String>,
}

#[derive(Clone)]
pub struct HelpRow {
    pub command: String,
    pub detail: String,
}

#[derive(Clone)]
pub struct StatusRow {
    pub label: String,
    pub value: String,
}

pub enum PromptOutcome {
    Line(String),
    Eof,
    Interrupted,
}

pub fn read_line_with_chrome(
    prompt: &str,
    prompt_width: usize,
    suggestions: &[Suggestion<'_>],
    chrome: &ComposerChrome,
) -> Result<PromptOutcome> {
    read_line(prompt, prompt_width, suggestions, Some(chrome))
}

fn read_line(
    prompt: &str,
    prompt_width: usize,
    suggestions: &[Suggestion<'_>],
    chrome: Option<&ComposerChrome>,
) -> Result<PromptOutcome> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        print!("{prompt}");
        io::stdout().flush()?;

        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 {
            return Ok(PromptOutcome::Eof);
        }

        return Ok(PromptOutcome::Line(input.trim().to_string()));
    }

    read_interactive_line(prompt, prompt_width, suggestions, chrome)
}

#[cfg(unix)]
fn read_interactive_line(
    prompt: &str,
    prompt_width: usize,
    suggestions: &[Suggestion<'_>],
    chrome: Option<&ComposerChrome>,
) -> Result<PromptOutcome> {
    let _raw = RawMode::enable()?;
    let mut input = Vec::new();
    let mut cursor = 0usize;
    let mut selected = 0usize;
    let mut menu_start = 0usize;
    let mut menu_hidden = false;

    render_prompt(
        prompt,
        prompt_width,
        &input,
        cursor,
        suggestions,
        selected,
        menu_start,
        chrome,
        menu_hidden,
        false,
    )?;

    loop {
        let matches = if menu_hidden {
            Vec::new()
        } else {
            matching_suggestions(&input, suggestions)
        };
        if selected >= matches.len() {
            selected = matches.len().saturating_sub(1);
        }
        clamp_menu_start(&mut menu_start, matches.len());

        match read_key()? {
            Key::Enter => {
                if !matches.is_empty() && command_picker_input(&input) {
                    let line = matches[selected].value.to_string();
                    if chrome.is_some_and(|chrome| is_help_command(&line, chrome)) {
                        clear_composer_area(chrome.is_some());
                        if let Some(chrome) = chrome {
                            show_help_view(chrome)?;
                        }
                        input.clear();
                        cursor = 0;
                        selected = 0;
                        menu_start = 0;
                        menu_hidden = false;
                        render_prompt(
                            prompt,
                            prompt_width,
                            &input,
                            cursor,
                            suggestions,
                            selected,
                            menu_start,
                            chrome,
                            menu_hidden,
                            false,
                        )?;
                        continue;
                    }
                    if chrome.is_some_and(|chrome| is_status_command(&line, chrome)) {
                        clear_composer_area(chrome.is_some());
                        if let Some(chrome) = chrome {
                            show_status_view(&chrome.status_rows)?;
                        }
                        input.clear();
                        cursor = 0;
                        selected = 0;
                        menu_start = 0;
                        menu_hidden = false;
                        render_prompt(
                            prompt,
                            prompt_width,
                            &input,
                            cursor,
                            suggestions,
                            selected,
                            menu_start,
                            chrome,
                            menu_hidden,
                            false,
                        )?;
                        continue;
                    }
                    clear_composer_area(chrome.is_some());
                    if let Some(chrome) = chrome {
                        render_static_composer(prompt, &line, chrome)?;
                    } else {
                        print!("{prompt}{line}\x1b[0m\n");
                    }
                    io::stdout().flush()?;
                    return Ok(PromptOutcome::Line(line));
                }

                let line = input.iter().collect::<String>().trim().to_string();
                if chrome.is_some_and(|chrome| is_help_command(&line, chrome)) {
                    clear_composer_area(chrome.is_some());
                    if let Some(chrome) = chrome {
                        show_help_view(chrome)?;
                    }
                    input.clear();
                    cursor = 0;
                    selected = 0;
                    menu_start = 0;
                    menu_hidden = false;
                    render_prompt(
                        prompt,
                        prompt_width,
                        &input,
                        cursor,
                        suggestions,
                        selected,
                        menu_start,
                        chrome,
                        menu_hidden,
                        false,
                    )?;
                    continue;
                }
                if chrome.is_some_and(|chrome| is_status_command(&line, chrome)) {
                    clear_composer_area(chrome.is_some());
                    if let Some(chrome) = chrome {
                        show_status_view(&chrome.status_rows)?;
                    }
                    input.clear();
                    cursor = 0;
                    selected = 0;
                    menu_start = 0;
                    menu_hidden = false;
                    render_prompt(
                        prompt,
                        prompt_width,
                        &input,
                        cursor,
                        suggestions,
                        selected,
                        menu_start,
                        chrome,
                        menu_hidden,
                        false,
                    )?;
                    continue;
                }
                clear_composer_area(chrome.is_some());
                if let Some(chrome) = chrome {
                    render_static_composer(prompt, &line, chrome)?;
                } else {
                    print!("{prompt}{line}\x1b[0m\n");
                }
                io::stdout().flush()?;
                return Ok(PromptOutcome::Line(line));
            }
            Key::Interrupt => {
                clear_composer_area(chrome.is_some());
                if let Some(chrome) = chrome {
                    render_static_composer(prompt, "", chrome)?;
                } else {
                    print!("{prompt}\x1b[0m\n");
                }
                io::stdout().flush()?;
                return Ok(PromptOutcome::Interrupted);
            }
            Key::EndOfInput if input.is_empty() => {
                clear_composer_area(chrome.is_some());
                println!();
                io::stdout().flush()?;
                return Ok(PromptOutcome::Eof);
            }
            Key::EndOfInput => {}
            Key::Backspace => {
                if cursor > 0 {
                    cursor -= 1;
                    input.remove(cursor);
                    selected = 0;
                    menu_start = 0;
                    menu_hidden = false;
                }
            }
            Key::Delete => {
                if cursor < input.len() {
                    input.remove(cursor);
                    selected = 0;
                    menu_start = 0;
                    menu_hidden = false;
                }
            }
            Key::DeleteToEnd => {
                input.truncate(cursor);
                selected = 0;
                menu_start = 0;
                menu_hidden = false;
            }
            Key::DeleteLine => {
                input.clear();
                cursor = 0;
                selected = 0;
                menu_start = 0;
                menu_hidden = false;
            }
            Key::DeletePreviousWord => {
                delete_previous_word(&mut input, &mut cursor);
                selected = 0;
                menu_start = 0;
                menu_hidden = false;
            }
            Key::Left => cursor = cursor.saturating_sub(1),
            Key::Right => {
                if cursor < input.len() {
                    cursor += 1;
                }
            }
            Key::Home => cursor = 0,
            Key::End => cursor = input.len(),
            Key::ClearScreen => {
                print!("\x1b[2J\x1b[H");
            }
            Key::Up if !matches.is_empty() => {
                selected = selected.checked_sub(1).unwrap_or(matches.len() - 1);
                if selected < menu_start {
                    menu_start = selected.saturating_sub(MAX_MENU_ROWS - 1);
                } else if selected == matches.len() - 1 {
                    menu_start = matches.len().saturating_sub(MAX_MENU_ROWS);
                }
            }
            Key::Down if !matches.is_empty() => {
                selected = (selected + 1) % matches.len();
                if selected == 0 {
                    menu_start = 0;
                } else if selected >= menu_start + MAX_MENU_ROWS {
                    menu_start = selected;
                }
            }
            Key::Tab if !matches.is_empty() => {
                input = autocomplete_command(matches[selected]);
                cursor = input.len();
                selected = 0;
                menu_start = 0;
                menu_hidden = true;
            }
            Key::Char(value) => {
                if value == '/' && !matches.is_empty() && command_picker_input(&input) {
                    input = autocomplete_command(matches[selected]);
                    cursor = input.len();
                    selected = 0;
                    menu_start = 0;
                    menu_hidden = true;
                } else {
                    input.insert(cursor, value);
                    cursor += 1;
                    selected = 0;
                    menu_start = 0;
                    menu_hidden = false;
                }
            }
            Key::Escape => {
                menu_hidden = true;
                selected = 0;
                menu_start = 0;
            }
            Key::Up | Key::Down | Key::Tab | Key::Other => {}
        }

        render_prompt(
            prompt,
            prompt_width,
            &input,
            cursor,
            suggestions,
            selected,
            menu_start,
            chrome,
            menu_hidden,
            true,
        )?;
    }
}

#[cfg(not(unix))]
fn read_interactive_line(
    prompt: &str,
    _prompt_width: usize,
    _suggestions: &[Suggestion<'_>],
    _chrome: Option<&ComposerChrome>,
) -> Result<PromptOutcome> {
    print!("{prompt}");
    io::stdout().flush()?;

    let mut input = String::new();
    if io::stdin().read_line(&mut input)? == 0 {
        return Ok(PromptOutcome::Eof);
    }

    Ok(PromptOutcome::Line(input.trim().to_string()))
}

fn render_prompt(
    prompt: &str,
    prompt_width: usize,
    input: &[char],
    cursor: usize,
    suggestions: &[Suggestion<'_>],
    selected: usize,
    menu_start: usize,
    chrome: Option<&ComposerChrome>,
    menu_hidden: bool,
    clear_existing: bool,
) -> Result<()> {
    let matches = if menu_hidden {
        Vec::new()
    } else {
        matching_suggestions(input, suggestions)
    };
    let input_text = styled_input(input, suggestions, chrome);
    let display_input = if input.is_empty() {
        placeholder_input(chrome)
    } else {
        input_text
    };
    let line_width = terminal_width().unwrap_or(DEFAULT_COMPOSER_WIDTH);
    let used_width = prompt_width + visible_chars(&display_input);
    let padding = line_width.saturating_sub(used_width);

    if clear_existing {
        clear_composer_area(chrome.is_some());
    }

    let footer_lines = if let Some(chrome) = chrome {
        render_rule(line_width);
        render_input_line(prompt, &display_input, padding);
        render_rule(line_width);
        render_footer(line_width, chrome, !matches.is_empty());
        3
    } else {
        print!("{prompt}{display_input}{}\x1b[0m", " ".repeat(padding));
        0
    };

    let visible_matches = visible_menu_rows(&matches, selected, menu_start);
    let menu_lines = visible_matches.len();
    for (suggestion, selected) in visible_matches {
        println!();
        print!("  {}", menu_row(suggestion, selected));
    }
    for _ in 0..COMPOSER_SAFE_AREA_LINES {
        println!();
    }

    if chrome.is_some() || menu_lines > 0 {
        let up_lines = menu_lines + footer_lines + COMPOSER_SAFE_AREA_LINES;
        if up_lines > 0 {
            print!("\x1b[{up_lines}A");
        }
        print!("\r");
        let column = prompt_width + input.iter().take(cursor).count();
        print!("\x1b[{column}C");
    } else {
        let back = padding + input.len().saturating_sub(cursor);
        if back > 0 {
            print!("\x1b[{back}D");
        }
    }

    io::stdout().flush()?;
    Ok(())
}

fn render_static_composer(prompt: &str, line: &str, chrome: &ComposerChrome) -> Result<()> {
    let line_width = terminal_width().unwrap_or(DEFAULT_COMPOSER_WIDTH);
    render_rule(line_width);
    let padding = line_width.saturating_sub(visible_chars(prompt) + line.chars().count());
    render_input_line(prompt, line, padding);
    render_rule(line_width);
    render_footer(line_width, chrome, false);
    io::stdout().flush()?;
    Ok(())
}

fn clear_composer_area(has_chrome: bool) {
    if has_chrome {
        print!("\r\x1b[1A\x1b[J");
    } else {
        print!("\r\x1b[J");
    }
}

fn render_input_line(prompt: &str, input_text: &str, padding: usize) {
    const INPUT_BG: &str = "\x1b[48;2;38;38;38m";
    let prompt = prompt.replace("\x1b[0m", "\x1b[0m\x1b[48;2;38;38;38m");
    print!(
        "{INPUT_BG}{prompt}{INPUT_BG}{input_text}{INPUT_BG}{}\x1b[0m\n",
        " ".repeat(padding)
    );
}

fn placeholder_input(chrome: Option<&ComposerChrome>) -> String {
    let Some(suggestion) = chrome
        .and_then(|chrome| chrome.prompt_suggestions.first())
        .filter(|suggestion| !suggestion.trim().is_empty())
    else {
        return String::new();
    };
    if color_enabled() {
        format!("\x1b[2m{suggestion}\x1b[0m")
    } else {
        suggestion.to_string()
    }
}

fn render_rule(width: usize) {
    let width = width.max(12);
    println!("\x1b[2m{}\x1b[0m", "─".repeat(width));
}

fn render_footer(width: usize, chrome: &ComposerChrome, picker_active: bool) {
    let left = &chrome.left_hint;
    let right = if picker_active {
        &chrome.picker_hint
    } else {
        &chrome.right_hint
    };
    let left_width = visible_chars(left);
    let right_width = visible_chars(right);
    let padding = width.saturating_sub(left_width + right_width + FOOTER_RIGHT_MARGIN);
    println!(
        "  {}\x1b[2m{}{}\x1b[0m",
        left,
        " ".repeat(padding.saturating_sub(2)),
        right
    );
}

fn show_help_view(chrome: &ComposerChrome) -> Result<()> {
    use crossterm::execute;
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
    use ratatui::backend::CrosstermBackend;
    use ratatui::Terminal;

    let _raw = RawMode::enable()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let result = (|| -> Result<()> {
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;

        loop {
            terminal.draw(|frame| draw_help_view(frame, chrome))?;
            match read_key()? {
                Key::Escape | Key::Interrupt => break,
                _ => {}
            }
        }

        Ok(())
    })();

    let mut stdout = io::stdout();
    execute!(stdout, LeaveAlternateScreen)?;
    result
}

pub fn show_status_view(rows: &[StatusRow]) -> Result<()> {
    use crossterm::execute;
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
    use ratatui::backend::CrosstermBackend;
    use ratatui::Terminal;

    let _raw = RawMode::enable()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let result = (|| -> Result<()> {
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;

        loop {
            terminal.draw(|frame| draw_status_view(frame, rows))?;
            match read_key()? {
                Key::Escape | Key::Interrupt | Key::Enter => break,
                _ => {}
            }
        }

        Ok(())
    })();

    let mut stdout = io::stdout();
    execute!(stdout, LeaveAlternateScreen)?;
    result
}

fn draw_help_view(frame: &mut ratatui::Frame<'_>, chrome: &ComposerChrome) {
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;

    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(area);

    let hint = Paragraph::new("Press esc to go back").style(Style::default().fg(Color::DarkGray));
    frame.render_widget(hint, rows[0]);

    let command_width = chrome
        .help_rows
        .iter()
        .map(|row| row.command.chars().count())
        .max()
        .unwrap_or(0)
        + 4;
    let lines = chrome
        .help_rows
        .iter()
        .map(|row| {
            Line::from(vec![
                Span::styled(
                    format!("  {:<width$}", row.command, width = command_width),
                    Style::default().fg(Color::Gray),
                ),
                Span::styled(&row.detail, Style::default().fg(Color::Gray)),
            ])
        })
        .collect::<Vec<_>>();
    let list = Paragraph::new(lines).style(Style::default().fg(Color::Gray));
    frame.render_widget(list, rows[2]);
}

fn draw_status_view(frame: &mut ratatui::Frame<'_>, rows: &[StatusRow]) {
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

    let area = frame.area();
    let rows_layout = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(4),
        ])
        .split(area);

    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            "Codeus status",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  Esc to go back", Style::default().fg(Color::DarkGray)),
    ]));
    frame.render_widget(title, rows_layout[0]);

    let summary = Paragraph::new("Session details that matter while you are building.")
        .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(summary, rows_layout[1]);

    let label_width = rows
        .iter()
        .map(|row| row.label.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let lines = rows
        .iter()
        .map(|row| {
            Line::from(vec![
                Span::styled(
                    format!("{:<label_width$}", row.label),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(&row.value, Style::default().fg(Color::Gray)),
            ])
        })
        .collect::<Vec<_>>();
    let panel = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::TOP | Borders::BOTTOM)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(panel, rows_layout[2]);
}

fn visible_menu_rows<'a>(
    matches: &[&'a Suggestion<'a>],
    selected: usize,
    menu_start: usize,
) -> Vec<(&'a Suggestion<'a>, bool)> {
    if matches.len() <= MAX_MENU_ROWS {
        return matches
            .iter()
            .enumerate()
            .map(|(index, suggestion)| (*suggestion, index == selected))
            .collect();
    }

    let max_start = matches.len().saturating_sub(MAX_MENU_ROWS);
    let start = menu_start.min(max_start);
    matches[start..start + MAX_MENU_ROWS]
        .iter()
        .enumerate()
        .map(|(offset, suggestion)| (*suggestion, start + offset == selected))
        .collect()
}

fn clamp_menu_start(menu_start: &mut usize, match_count: usize) {
    if match_count <= MAX_MENU_ROWS {
        *menu_start = 0;
        return;
    }

    *menu_start = (*menu_start).min(match_count - MAX_MENU_ROWS);
}

fn visible_chars(value: &str) -> usize {
    let mut count = 0usize;
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            count += 1;
        }
    }
    count
}

fn terminal_width() -> Option<usize> {
    terminal_width_from_ioctl().or_else(|| {
        std::env::var("COLUMNS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|width| *width >= 40)
    })
}

#[cfg(unix)]
fn terminal_width_from_ioctl() -> Option<usize> {
    let mut winsize = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let result = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut winsize) };
    (result == 0 && winsize.ws_col >= 40).then_some(winsize.ws_col as usize)
}

#[cfg(not(unix))]
fn terminal_width_from_ioctl() -> Option<usize> {
    None
}

fn color_enabled() -> bool {
    io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn menu_row(suggestion: &Suggestion<'_>, selected: bool) -> String {
    let command = format!("{:<width$}", suggestion.value, width = COMMAND_COLUMN_WIDTH);
    let row = format!("{command} {}", suggestion.detail);
    if color_enabled() {
        if selected {
            format!("{ACTIVE_COLOR}{row}\x1b[0m")
        } else {
            format!(
                "\x1b[38;2;180;180;180m{command}\x1b[0m \x1b[2m{}\x1b[0m",
                suggestion.detail
            )
        }
    } else {
        row
    }
}

fn styled_input(
    input: &[char],
    suggestions: &[Suggestion<'_>],
    chrome: Option<&ComposerChrome>,
) -> String {
    let input_text = input.iter().collect::<String>();
    if let Some(command) = detected_command(&input_text, suggestions, chrome) {
        let rest = &input_text[command.len()..];
        format!("{ACTIVE_COLOR}{command}\x1b[0m{rest}")
    } else {
        input_text
    }
}

fn detected_command<'a>(
    input_text: &'a str,
    suggestions: &[Suggestion<'_>],
    chrome: Option<&ComposerChrome>,
) -> Option<&'a str> {
    if !input_text.starts_with('/') || !color_enabled() {
        return None;
    }

    let command_end = input_text
        .char_indices()
        .find_map(|(index, ch)| ch.is_whitespace().then_some(index))
        .unwrap_or(input_text.len());
    let command = &input_text[..command_end];
    if command.is_empty() {
        return None;
    }

    let known = suggestions
        .iter()
        .any(|suggestion| suggestion.value.eq_ignore_ascii_case(command))
        || chrome.is_some_and(|chrome| {
            chrome
                .active_commands
                .iter()
                .any(|known| known.eq_ignore_ascii_case(command))
        });
    known.then_some(command)
}

fn matching_suggestions<'a>(
    input: &[char],
    suggestions: &'a [Suggestion<'a>],
) -> Vec<&'a Suggestion<'a>> {
    if input.is_empty() || input[0] != '/' {
        return Vec::new();
    }

    let text = input.iter().collect::<String>();
    if text.chars().any(char::is_whitespace) {
        return Vec::new();
    }

    let text_lower = text.to_ascii_lowercase();
    let exact = suggestions
        .iter()
        .filter(|suggestion| suggestion.value.eq_ignore_ascii_case(&text));
    let prefix = suggestions.iter().filter(|suggestion| {
        let value = suggestion.value.to_ascii_lowercase();
        value.starts_with(&text_lower) && !suggestion.value.eq_ignore_ascii_case(&text)
    });

    exact.chain(prefix).collect()
}

fn command_picker_input(input: &[char]) -> bool {
    !input.is_empty() && input[0] == '/' && !input.iter().any(|ch| ch.is_whitespace())
}

fn is_help_command(input_text: &str, chrome: &ComposerChrome) -> bool {
    let Some(command) = input_text.split_whitespace().next() else {
        return false;
    };

    ["/help", "/commands", "/?"]
        .iter()
        .any(|known| known.eq_ignore_ascii_case(command))
        && chrome
            .active_commands
            .iter()
            .any(|known| known.eq_ignore_ascii_case(command))
}

fn is_status_command(input_text: &str, chrome: &ComposerChrome) -> bool {
    let Some(command) = input_text.split_whitespace().next() else {
        return false;
    };

    command.eq_ignore_ascii_case("/status")
        && chrome
            .active_commands
            .iter()
            .any(|known| known.eq_ignore_ascii_case(command))
}

fn autocomplete_command(suggestion: &Suggestion<'_>) -> Vec<char> {
    format!("{} ", suggestion.value).chars().collect()
}

fn delete_previous_word(input: &mut Vec<char>, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }

    let mut start = *cursor;
    while start > 0 && input[start - 1].is_whitespace() {
        start -= 1;
    }
    while start > 0 && !input[start - 1].is_whitespace() {
        start -= 1;
    }

    input.drain(start..*cursor);
    *cursor = start;
}

#[derive(Debug, Clone, Copy)]
enum Key {
    Char(char),
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    DeleteToEnd,
    DeleteLine,
    DeletePreviousWord,
    ClearScreen,
    Enter,
    Escape,
    Interrupt,
    EndOfInput,
    Tab,
    Other,
}

#[cfg(unix)]
fn read_key() -> Result<Key> {
    let mut stdin = io::stdin().lock();
    let Some(byte) = read_byte_blocking(&mut stdin)? else {
        return Ok(Key::Other);
    };

    match byte {
        b'\n' | b'\r' => Ok(Key::Enter),
        b'\t' => Ok(Key::Tab),
        0x01 => Ok(Key::Home),
        0x02 => Ok(Key::Left),
        0x03 => Ok(Key::Interrupt),
        0x04 => Ok(Key::EndOfInput),
        0x05 => Ok(Key::End),
        0x06 => Ok(Key::Right),
        0x0b => Ok(Key::DeleteToEnd),
        0x0c => Ok(Key::ClearScreen),
        0x0e => Ok(Key::Down),
        0x10 => Ok(Key::Up),
        0x15 => Ok(Key::DeleteLine),
        0x17 => Ok(Key::DeletePreviousWord),
        0x08 | 0x7f => Ok(Key::Backspace),
        0x1b => read_escape_key(&mut stdin),
        b' '..=b'~' => Ok(Key::Char(byte as char)),
        _ => Ok(Key::Other),
    }
}

#[cfg(unix)]
fn read_escape_key<R: Read>(reader: &mut R) -> Result<Key> {
    let sequence = read_escape_sequence(reader)?;
    if sequence.is_empty() {
        return Ok(Key::Escape);
    }

    match sequence.as_slice() {
        [b'[', b'A'] => Ok(Key::Up),
        [b'[', b'B'] => Ok(Key::Down),
        [b'[', b'C'] => Ok(Key::Right),
        [b'[', b'D'] => Ok(Key::Left),
        [b'[', b'H'] | [b'O', b'H'] => Ok(Key::Home),
        [b'[', b'F'] | [b'O', b'F'] => Ok(Key::End),
        [b'[', b'3', b'~'] => Ok(Key::Delete),
        _ => Ok(Key::Other),
    }
}

#[cfg(unix)]
fn read_escape_sequence<R: Read>(reader: &mut R) -> Result<Vec<u8>> {
    let mut sequence = Vec::with_capacity(8);

    for _ in 0..8 {
        let Some(byte) = read_byte_timeout(reader)? else {
            break;
        };
        sequence.push(byte);
        if byte.is_ascii_alphabetic() || byte == b'~' {
            break;
        }
    }

    Ok(sequence)
}

#[cfg(unix)]
fn read_byte_blocking<R: Read>(reader: &mut R) -> Result<Option<u8>> {
    loop {
        if let Some(byte) = read_byte_timeout(reader)? {
            return Ok(Some(byte));
        }
    }
}

#[cfg(unix)]
fn read_byte_timeout<R: Read>(reader: &mut R) -> Result<Option<u8>> {
    let mut byte = [0u8; 1];
    let read = reader.read(&mut byte)?;
    Ok((read == 1).then_some(byte[0]))
}

#[cfg(unix)]
struct RawMode {
    original: libc::termios,
}

#[cfg(unix)]
impl RawMode {
    fn enable() -> Result<Self> {
        let fd = libc::STDIN_FILENO;
        let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();

        unsafe {
            if libc::tcgetattr(fd, original.as_mut_ptr()) != 0 {
                bail!("failed to read terminal mode");
            }
            let original = original.assume_init();
            let mut raw = original;
            raw.c_lflag &= !(libc::ICANON | libc::ECHO);
            raw.c_iflag &= !(libc::IXON | libc::ICRNL);
            raw.c_cc[libc::VMIN] = 0;
            raw.c_cc[libc::VTIME] = 1;

            if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
                bail!("failed to enter raw terminal mode");
            }

            Ok(Self { original })
        }
    }
}

#[cfg(unix)]
impl Drop for RawMode {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUGGESTIONS: &[Suggestion<'static>] = &[
        Suggestion {
            value: "/help",
            detail: "Show help",
        },
        Suggestion {
            value: "/status",
            detail: "Show status",
        },
        Suggestion {
            value: "/swap",
            detail: "Show swap helper",
        },
    ];

    #[test]
    fn slash_matches_every_command() {
        let input = vec!['/'];

        let matches = matching_suggestions(&input, SUGGESTIONS);

        assert_eq!(matches.len(), SUGGESTIONS.len());
        assert_eq!(matches[0].value, "/help");
        assert_eq!(matches[2].value, "/swap");
    }

    #[test]
    fn prefix_match_keeps_command_order() {
        let input = "/s".chars().collect::<Vec<_>>();

        let matches = matching_suggestions(&input, SUGGESTIONS);

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].value, "/status");
        assert_eq!(matches[1].value, "/swap");
    }

    #[test]
    fn visible_menu_uses_stable_window() {
        let matches = SUGGESTIONS.iter().collect::<Vec<_>>();

        let rows = visible_menu_rows(&matches, 1, 0);

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0.value, "/help");
        assert!(rows[1].1);
    }

    #[test]
    fn visible_menu_respects_window_start() {
        const MANY_SUGGESTIONS: &[Suggestion<'static>] = &[
            Suggestion {
                value: "/one",
                detail: "One",
            },
            Suggestion {
                value: "/two",
                detail: "Two",
            },
            Suggestion {
                value: "/three",
                detail: "Three",
            },
            Suggestion {
                value: "/four",
                detail: "Four",
            },
            Suggestion {
                value: "/five",
                detail: "Five",
            },
            Suggestion {
                value: "/six",
                detail: "Six",
            },
            Suggestion {
                value: "/seven",
                detail: "Seven",
            },
        ];
        let matches = MANY_SUGGESTIONS.iter().collect::<Vec<_>>();

        let rows = visible_menu_rows(&matches, 5, 5);

        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].0.value, "/three");
        assert_eq!(rows[4].0.value, "/seven");
        assert!(rows[3].1);
    }
}
