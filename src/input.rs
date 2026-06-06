use anyhow::Result;
use std::io::{self, IsTerminal, Write};

const DEFAULT_COMPOSER_WIDTH: usize = 120;
const COMMAND_COLUMN_WIDTH: usize = 18;
const MAX_MENU_ROWS: usize = 5;
const COMPOSER_SAFE_AREA_LINES: usize = 2;
const COMPOSER_BAND_LINES: usize = 3;
const COMPOSER_INPUT_LINE_OFFSET: usize = 1;
const ACTIVE_COLOR: &str = "\x1b[1m\x1b[38;2;0;168;204m";
const INPUT_BG: &str = "\x1b[48;2;48;48;48m";

#[derive(Debug, Clone, Copy)]
pub struct Suggestion<'a> {
    pub value: &'a str,
    pub detail: &'a str,
}

pub struct ComposerChrome {
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

#[derive(Default)]
pub struct InputHistory {
    entries: Vec<String>,
    cursor: Option<usize>,
    draft: Option<String>,
}

impl InputHistory {
    pub fn record(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        if self.entries.last().is_some_and(|entry| entry == line) {
            self.reset_navigation();
            return;
        }
        self.entries.push(line.to_string());
        self.reset_navigation();
    }

    fn previous(&mut self, current: &[char]) -> Option<Vec<char>> {
        if self.entries.is_empty() {
            return None;
        }

        let next = match self.cursor {
            Some(0) => 0,
            Some(index) => index - 1,
            None => {
                self.draft = Some(current.iter().collect());
                self.entries.len() - 1
            }
        };
        self.cursor = Some(next);
        Some(self.entries[next].chars().collect())
    }

    fn next(&mut self) -> Option<Vec<char>> {
        let cursor = self.cursor?;
        if cursor + 1 < self.entries.len() {
            let next = cursor + 1;
            self.cursor = Some(next);
            return Some(self.entries[next].chars().collect());
        }

        self.cursor = None;
        Some(self.draft.take().unwrap_or_default().chars().collect())
    }

    fn reset_navigation(&mut self) {
        self.cursor = None;
        self.draft = None;
    }
}

pub fn read_line_with_chrome_and_history(
    prompt: &str,
    prompt_width: usize,
    suggestions: &[Suggestion<'_>],
    chrome: &ComposerChrome,
    history: &mut InputHistory,
) -> Result<PromptOutcome> {
    read_line(
        prompt,
        prompt_width,
        suggestions,
        Some(chrome),
        Some(history),
    )
}

fn read_line(
    prompt: &str,
    prompt_width: usize,
    suggestions: &[Suggestion<'_>],
    chrome: Option<&ComposerChrome>,
    mut history: Option<&mut InputHistory>,
) -> Result<PromptOutcome> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        print!("{prompt}");
        io::stdout().flush()?;

        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 {
            return Ok(PromptOutcome::Eof);
        }

        let line = input.trim().to_string();
        if let Some(history) = history.as_mut() {
            history.record(&line);
        }
        return Ok(PromptOutcome::Line(line));
    }

    read_interactive_line(prompt, prompt_width, suggestions, chrome, history)
}

#[cfg(any(unix, windows))]
fn read_interactive_line(
    prompt: &str,
    prompt_width: usize,
    suggestions: &[Suggestion<'_>],
    chrome: Option<&ComposerChrome>,
    mut history: Option<&mut InputHistory>,
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
                        print!("{prompt}{line}\x1b[0m\r\n");
                    }
                    io::stdout().flush()?;
                    if let Some(history) = history.as_mut() {
                        history.record(&line);
                    }
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
                    print!("{prompt}{line}\x1b[0m\r\n");
                }
                io::stdout().flush()?;
                if let Some(history) = history.as_mut() {
                    history.record(&line);
                }
                return Ok(PromptOutcome::Line(line));
            }
            Key::Interrupt => {
                clear_composer_area(chrome.is_some());
                if let Some(chrome) = chrome {
                    render_static_composer(prompt, "", chrome)?;
                } else {
                    print!("{prompt}\x1b[0m\r\n");
                }
                io::stdout().flush()?;
                return Ok(PromptOutcome::Interrupted);
            }
            Key::EndOfInput if input.is_empty() => {
                clear_composer_area(chrome.is_some());
                print!("\r\n");
                io::stdout().flush()?;
                return Ok(PromptOutcome::Eof);
            }
            Key::EndOfInput => {}
            Key::Backspace => {
                if cursor > 0 {
                    cursor -= 1;
                    input.remove(cursor);
                    if let Some(history) = history.as_mut() {
                        history.reset_navigation();
                    }
                    selected = 0;
                    menu_start = 0;
                    menu_hidden = false;
                }
            }
            Key::Delete => {
                if cursor < input.len() {
                    input.remove(cursor);
                    if let Some(history) = history.as_mut() {
                        history.reset_navigation();
                    }
                    selected = 0;
                    menu_start = 0;
                    menu_hidden = false;
                }
            }
            Key::DeleteToEnd => {
                input.truncate(cursor);
                if let Some(history) = history.as_mut() {
                    history.reset_navigation();
                }
                selected = 0;
                menu_start = 0;
                menu_hidden = false;
            }
            Key::DeleteLine => {
                input.clear();
                cursor = 0;
                if let Some(history) = history.as_mut() {
                    history.reset_navigation();
                }
                selected = 0;
                menu_start = 0;
                menu_hidden = false;
            }
            Key::DeletePreviousWord => {
                delete_previous_word(&mut input, &mut cursor);
                if let Some(history) = history.as_mut() {
                    history.reset_navigation();
                }
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
                if let Some(history) = history.as_mut() {
                    history.reset_navigation();
                }
                selected = 0;
                menu_start = 0;
                menu_hidden = true;
            }
            Key::Tab => {
                if let Some(suggestion) = matching_prompt_suggestion(&input, chrome) {
                    input = suggestion.chars().collect();
                    cursor = input.len();
                    if let Some(history) = history.as_mut() {
                        history.reset_navigation();
                    }
                    selected = 0;
                    menu_start = 0;
                    menu_hidden = true;
                }
            }
            Key::Char(value) => {
                if value == '/' && !matches.is_empty() && command_picker_input(&input) {
                    input = autocomplete_command(matches[selected]);
                    cursor = input.len();
                    if let Some(history) = history.as_mut() {
                        history.reset_navigation();
                    }
                    selected = 0;
                    menu_start = 0;
                    menu_hidden = true;
                } else {
                    input.insert(cursor, value);
                    cursor += 1;
                    if let Some(history) = history.as_mut() {
                        history.reset_navigation();
                    }
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
            Key::Up => {
                if let Some(next) = history
                    .as_mut()
                    .and_then(|history| history.previous(&input))
                {
                    input = next;
                    cursor = input.len();
                    selected = 0;
                    menu_start = 0;
                    menu_hidden = true;
                }
            }
            Key::Down => {
                if let Some(next) = history.as_mut().and_then(|history| history.next()) {
                    input = next;
                    cursor = input.len();
                    selected = 0;
                    menu_start = 0;
                    menu_hidden = true;
                }
            }
            Key::Other => {}
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

#[cfg(not(any(unix, windows)))]
fn read_interactive_line(
    prompt: &str,
    _prompt_width: usize,
    _suggestions: &[Suggestion<'_>],
    _chrome: Option<&ComposerChrome>,
    mut history: Option<&mut InputHistory>,
) -> Result<PromptOutcome> {
    print!("{prompt}");
    io::stdout().flush()?;

    let mut input = String::new();
    if io::stdin().read_line(&mut input)? == 0 {
        return Ok(PromptOutcome::Eof);
    }

    let line = input.trim().to_string();
    if let Some(history) = history.as_mut() {
        history.record(&line);
    }
    Ok(PromptOutcome::Line(line))
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
    let line_width = composer_draw_width(terminal_width().unwrap_or(DEFAULT_COMPOSER_WIDTH));
    let used_width = prompt_width + visible_chars(&display_input);
    let padding = line_width.saturating_sub(used_width);

    if clear_existing {
        clear_composer_area(chrome.is_some());
    }

    let input_lines = if chrome.is_some() {
        render_input_band(prompt, &display_input, line_width);
        COMPOSER_BAND_LINES
    } else {
        print!("{prompt}{display_input}{}\x1b[0m", " ".repeat(padding));
        0
    };

    let visible_matches = visible_menu_rows(&matches, selected, menu_start);
    let menu_lines = visible_matches.len();
    for (suggestion, selected) in visible_matches {
        print!("\r\n");
        print!("  {}", menu_row(suggestion, selected));
    }
    for _ in 0..COMPOSER_SAFE_AREA_LINES {
        print!("\r\n");
    }

    // Reposition cursor on the input line.
    //
    // After rendering, the cursor has moved down from the band start:
    //   input_lines  (band rows, each ending with \r\n)
    //   + menu_lines  (autocomplete menu rows)
    //   + SAFE_AREA   (blank rows below the menu)
    //
    // The input row sits at offset COMPOSER_INPUT_LINE_OFFSET from the
    // band start, so we go up by:
    //   total_rows_down − OFFSET
    //
    // When there is no band (input_lines == 0), saturating_sub yields 0,
    // which correctly keeps the cursor on the inline prompt row.
    if chrome.is_some() || menu_lines > 0 {
        let up_lines = menu_lines
            + COMPOSER_SAFE_AREA_LINES
            + input_lines.saturating_sub(COMPOSER_INPUT_LINE_OFFSET);
        if up_lines > 0 {
            print!("\x1b[{up_lines}A");
        }
        print!("\r");
        let column =
            (prompt_width + input.iter().take(cursor).count()).min(line_width.saturating_sub(1));
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

fn render_static_composer(prompt: &str, line: &str, _chrome: &ComposerChrome) -> Result<()> {
    let line_width = composer_draw_width(terminal_width().unwrap_or(DEFAULT_COMPOSER_WIDTH));
    render_input_band(prompt, line, line_width);
    io::stdout().flush()?;
    Ok(())
}

/// Move from the input row back to the top of the band, then erase
/// everything below.  The input row is `COMPOSER_INPUT_LINE_OFFSET`
/// lines below the band top.
fn clear_composer_area(has_chrome: bool) {
    if has_chrome {
        print!("\r\x1b[{COMPOSER_INPUT_LINE_OFFSET}A\x1b[J");
    } else {
        print!("\r\x1b[J");
    }
}

fn render_input_band(prompt: &str, input_text: &str, width: usize) {
    print!("{}", input_band(prompt, input_text, width));
}

fn input_band(prompt: &str, input_text: &str, width: usize) -> String {
    format!(
        "{}{}{}",
        blank_input_line(),
        input_line(prompt, input_text, width),
        blank_input_line()
    )
}

/// Render a full-width blank line with the input background color.
/// Uses `\x1b[K` (erase-to-end-of-line) so the background extends to
/// the terminal edge without manual space-padding or wrap-column math.
fn blank_input_line() -> String {
    format!("{INPUT_BG}\x1b[K\x1b[0m\r\n")
}

/// Render the input line with prompt and text, filling the rest of the
/// line with `\x1b[K`.  The `width` parameter is only used to truncate
/// text that would exceed the terminal width; the visual fill is
/// handled by the erase-to-end-of-line escape.
fn input_line(prompt: &str, input_text: &str, width: usize) -> String {
    let prompt = paint_on_input_bg(prompt);
    let input_text = truncate_visible(input_text, width.saturating_sub(visible_chars(&prompt)));
    let input_text = paint_on_input_bg(&input_text);
    format!(
        "{INPUT_BG}{prompt}{INPUT_BG}{input_text}{INPUT_BG}\x1b[K\x1b[0m\r\n"
    )
}

fn composer_draw_width(terminal_width: usize) -> usize {
    terminal_width.saturating_sub(1).max(1)
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

fn paint_on_input_bg(value: &str) -> String {
    value.replace("\x1b[0m", &format!("\x1b[0m{INPUT_BG}"))
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

fn truncate_visible(value: &str, max_width: usize) -> String {
    let mut output = String::new();
    let mut width = 0usize;
    let mut saw_escape = false;
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            saw_escape = true;
            output.push(ch);
            output.push(chars.next().expect("peeked escape bracket"));
            for next in chars.by_ref() {
                output.push(next);
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if width < max_width {
            output.push(ch);
            width += 1;
        } else {
            break;
        }
    }

    if saw_escape && !output.ends_with("\x1b[0m") {
        output.push_str("\x1b[0m");
    }

    output
}

fn terminal_width() -> Option<usize> {
    crossterm::terminal::size()
        .ok()
        .map(|(width, _)| width as usize)
        .filter(|width| *width >= 40)
        .or_else(|| {
            std::env::var("COLUMNS")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|width| *width >= 40)
        })
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

fn matching_prompt_suggestion<'a>(
    input: &[char],
    chrome: Option<&'a ComposerChrome>,
) -> Option<&'a str> {
    let chrome = chrome?;
    let text = input.iter().collect::<String>();
    if text.starts_with('/') {
        return None;
    }
    let text = text.trim_start();
    chrome
        .prompt_suggestions
        .iter()
        .find(|suggestion| {
            !suggestion.trim().is_empty()
                && (text.is_empty()
                    || suggestion
                        .to_ascii_lowercase()
                        .starts_with(&text.to_ascii_lowercase()))
        })
        .map(String::as_str)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

fn read_key() -> Result<Key> {
    use crossterm::event::{Event, KeyEventKind};

    loop {
        let event = crossterm::event::read()?;
        if let Event::Key(event) = event {
            if !matches!(event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                continue;
            }
            return Ok(key_from_event(event));
        }
    }
}

fn key_from_event(event: crossterm::event::KeyEvent) -> Key {
    use crossterm::event::{KeyCode, KeyModifiers};

    if event.modifiers.contains(KeyModifiers::CONTROL) {
        match event.code {
            KeyCode::Char('a' | 'A') => return Key::Home,
            KeyCode::Char('b' | 'B') => return Key::Left,
            KeyCode::Char('c' | 'C') => return Key::Interrupt,
            KeyCode::Char('d' | 'D') => return Key::EndOfInput,
            KeyCode::Char('e' | 'E') => return Key::End,
            KeyCode::Char('f' | 'F') => return Key::Right,
            KeyCode::Char('k' | 'K') => return Key::DeleteToEnd,
            KeyCode::Char('l' | 'L') => return Key::ClearScreen,
            KeyCode::Char('n' | 'N') => return Key::Down,
            KeyCode::Char('p' | 'P') => return Key::Up,
            KeyCode::Char('u' | 'U') => return Key::DeleteLine,
            KeyCode::Char('w' | 'W') => return Key::DeletePreviousWord,
            _ => {}
        }
    }

    match event.code {
        KeyCode::Char(value) => Key::Char(value),
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab => Key::Tab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::Esc => Key::Escape,
        _ => Key::Other,
    }
}

struct RawMode {
    enabled: bool,
}

impl RawMode {
    fn enable() -> Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self { enabled: true })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if self.enabled {
            let _ = crossterm::terminal::disable_raw_mode();
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
    fn empty_input_matches_first_prompt_suggestion() {
        let chrome = ComposerChrome {
            active_commands: Vec::new(),
            help_rows: Vec::new(),
            status_rows: Vec::new(),
            prompt_suggestions: vec![
                "Write tests for @filename".to_string(),
                "Review this repo".to_string(),
            ],
        };

        let suggestion = matching_prompt_suggestion(&[], Some(&chrome));

        assert_eq!(suggestion, Some("Write tests for @filename"));
    }

    #[test]
    fn prompt_suggestion_matches_typed_prefix() {
        let chrome = ComposerChrome {
            active_commands: Vec::new(),
            help_rows: Vec::new(),
            status_rows: Vec::new(),
            prompt_suggestions: vec![
                "Build an Omniston v1beta8 quote flow".to_string(),
                "Review this repo".to_string(),
            ],
        };
        let input = "build an".chars().collect::<Vec<_>>();

        let suggestion = matching_prompt_suggestion(&input, Some(&chrome));

        assert_eq!(suggestion, Some("Build an Omniston v1beta8 quote flow"));
    }

    #[test]
    fn prompt_suggestion_ignores_slash_commands() {
        let chrome = ComposerChrome {
            active_commands: Vec::new(),
            help_rows: Vec::new(),
            status_rows: Vec::new(),
            prompt_suggestions: vec!["Review this repo".to_string()],
        };
        let input = "/r".chars().collect::<Vec<_>>();

        let suggestion = matching_prompt_suggestion(&input, Some(&chrome));

        assert_eq!(suggestion, None);
    }

    #[test]
    fn key_mapping_is_platform_neutral_for_arrows() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let key = key_from_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));

        assert_eq!(key, Key::Up);
    }

    #[test]
    fn key_mapping_keeps_control_shortcuts_consistent() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let key = key_from_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));

        assert_eq!(key, Key::DeletePreviousWord);
    }

    #[test]
    fn input_history_walks_previous_and_restores_draft() {
        let mut history = InputHistory::default();
        history.record("first");
        history.record("second");

        let draft = "draft".chars().collect::<Vec<_>>();

        assert_eq!(
            history.previous(&draft),
            Some("second".chars().collect::<Vec<_>>())
        );
        assert_eq!(
            history.previous(&[]),
            Some("first".chars().collect::<Vec<_>>())
        );
        assert_eq!(
            history.previous(&[]),
            Some("first".chars().collect::<Vec<_>>())
        );
        assert_eq!(history.next(), Some("second".chars().collect::<Vec<_>>()));
        assert_eq!(history.next(), Some("draft".chars().collect::<Vec<_>>()));
        assert_eq!(history.next(), None);
    }

    #[test]
    fn input_history_skips_empty_and_adjacent_duplicates() {
        let mut history = InputHistory::default();
        history.record("");
        history.record("same");
        history.record("same");

        assert_eq!(
            history.previous(&[]),
            Some("same".chars().collect::<Vec<_>>())
        );
        assert_eq!(history.next(), Some(Vec::new()));
    }

    #[test]
    fn composer_draw_width_avoids_terminal_wrap_column() {
        assert_eq!(composer_draw_width(120), 119);
        assert_eq!(composer_draw_width(1), 1);
    }

    #[test]
    fn input_band_uses_erase_for_full_width_fill() {
        let band = input_band("> ", "hello", 16);

        assert_eq!(band.matches('\n').count(), COMPOSER_BAND_LINES);
        // Each line uses \x1b[K to fill remaining width instead of
        // manual space-padding, avoiding wrap-column arithmetic.
        assert!(band.contains("\x1b[K"));
        // The input line still renders prompt + text.
        let content_line = band.lines().nth(1).unwrap();
        assert!(content_line.contains("hello"));
    }

    #[test]
    fn input_band_truncates_long_visible_text_before_wrap_column() {
        let band = input_band("> ", "hello world", 8);
        let input_line = band.lines().nth(1).unwrap();

        assert_eq!(visible_chars(input_line), 8);
        assert!(input_line.contains("hello "));
        assert!(!input_line.contains("world"));
    }

    #[test]
    fn input_band_truncates_ansi_styled_text_by_visible_width() {
        let band = input_band("> ", "\x1b[2mhello world\x1b[0m", 8);
        let input_line = band.lines().nth(1).unwrap();

        assert_eq!(visible_chars(input_line), 8);
        assert!(input_line.contains("\x1b[2m"));
        assert!(input_line.contains("\x1b[0m"));
        assert!(input_line.contains("hello "));
        assert!(!input_line.contains("world"));
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
