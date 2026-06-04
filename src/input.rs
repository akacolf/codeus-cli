use anyhow::{bail, Result};
use std::io::{self, IsTerminal, Read, Write};

const MAX_VISIBLE_SUGGESTIONS: usize = 12;

#[derive(Debug, Clone, Copy)]
pub struct Suggestion<'a> {
    pub value: &'a str,
    pub detail: &'a str,
}

pub enum PromptOutcome {
    Line(String),
    Eof,
    Interrupted,
}

pub fn read_line_with_suggestions(
    prompt: &str,
    prompt_width: usize,
    suggestions: &[Suggestion<'_>],
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

    read_interactive_line(prompt, prompt_width, suggestions)
}

#[cfg(unix)]
fn read_interactive_line(
    prompt: &str,
    prompt_width: usize,
    suggestions: &[Suggestion<'_>],
) -> Result<PromptOutcome> {
    let _raw = RawMode::enable()?;
    let mut input = Vec::new();
    let mut cursor = 0usize;
    let mut selected = 0usize;

    print!("\x1b7");
    render_prompt(prompt, prompt_width, &input, cursor, suggestions, selected)?;

    loop {
        let matches = matching_suggestions(&input, suggestions);
        if selected >= matches.len() {
            selected = matches.len().saturating_sub(1);
        }

        match read_key()? {
            Key::Enter => {
                let line = input.iter().collect::<String>().trim().to_string();
                print!("\x1b8\x1b[J{prompt}{line}\n");
                io::stdout().flush()?;
                return Ok(PromptOutcome::Line(line));
            }
            Key::Interrupt => {
                print!("\x1b8\x1b[J{prompt}\n");
                io::stdout().flush()?;
                return Ok(PromptOutcome::Interrupted);
            }
            Key::EndOfInput if input.is_empty() => {
                print!("\x1b8\x1b[J\n");
                io::stdout().flush()?;
                return Ok(PromptOutcome::Eof);
            }
            Key::EndOfInput => {}
            Key::Backspace => {
                if cursor > 0 {
                    cursor -= 1;
                    input.remove(cursor);
                    selected = 0;
                }
            }
            Key::Delete => {
                if cursor < input.len() {
                    input.remove(cursor);
                    selected = 0;
                }
            }
            Key::Left => cursor = cursor.saturating_sub(1),
            Key::Right => {
                if cursor < input.len() {
                    cursor += 1;
                }
            }
            Key::Home => cursor = 0,
            Key::End => cursor = input.len(),
            Key::Up if !matches.is_empty() => {
                selected = selected.checked_sub(1).unwrap_or(matches.len() - 1);
            }
            Key::Down if !matches.is_empty() => selected = (selected + 1) % matches.len(),
            Key::Tab if !matches.is_empty() => {
                input = matches[selected].value.chars().collect();
                cursor = input.len();
                selected = 0;
            }
            Key::Char(value) => {
                input.insert(cursor, value);
                cursor += 1;
                selected = 0;
            }
            Key::Escape => {
                input.clear();
                cursor = 0;
                selected = 0;
            }
            Key::Up | Key::Down | Key::Tab | Key::Other => {}
        }

        render_prompt(prompt, prompt_width, &input, cursor, suggestions, selected)?;
    }
}

#[cfg(not(unix))]
fn read_interactive_line(
    prompt: &str,
    _prompt_width: usize,
    _suggestions: &[Suggestion<'_>],
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
) -> Result<()> {
    let input_text = input.iter().collect::<String>();
    let matches = matching_suggestions(input, suggestions);

    print!("\x1b8\x1b[J{prompt}{input_text}");

    for (index, suggestion) in matches.iter().enumerate() {
        let marker = if index == selected { ">" } else { " " };
        println!();
        print!("{marker} {:<13} {}", suggestion.value, suggestion.detail);
    }

    let menu_lines = matches.len();
    if menu_lines > 0 {
        print!("\x1b[{menu_lines}A\r");
        let column = prompt_width + input.iter().take(cursor).count();
        if column > 0 {
            print!("\x1b[{column}C");
        }
    } else if cursor < input.len() {
        print!("\x1b[{}D", input.len() - cursor);
    }

    io::stdout().flush()?;
    Ok(())
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

    suggestions
        .iter()
        .filter(|suggestion| suggestion.value.starts_with(&text))
        .take(MAX_VISIBLE_SUGGESTIONS)
        .collect()
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
        0x03 => Ok(Key::Interrupt),
        0x04 => Ok(Key::EndOfInput),
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
