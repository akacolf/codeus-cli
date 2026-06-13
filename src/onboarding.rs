use crate::auth;
use crate::config::Config;
use crate::terminal::{theme_label, Terminal, THEME_CHOICES};
use anyhow::{bail, Context, Result};
#[cfg(unix)]
use std::io::Read;
use std::io::{self, IsTerminal, Write};
use std::process;

pub fn should_run(config: &Config) -> bool {
    !has_current_session(config) && io::stdin().is_terminal() && io::stdout().is_terminal()
}

fn has_current_session(config: &Config) -> bool {
    config
        .session_token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty())
        && !auth::token_is_expired(config)
}

pub async fn run(
    api_base: Option<String>,
    model: Option<String>,
    mut config: Config,
) -> Result<Config> {
    choose_theme(&mut config)?;
    let terminal = Terminal::detect_with_theme(config.theme.as_deref());

    let login_choices = ["Sign in with Quantum Club"];
    let login_choice = select_index(
        &terminal,
        "Sign in to continue",
        "Connect your Quantum Club account in the browser.",
        &login_choices,
        0,
        SelectorStyle::Login,
        |_| String::new(),
    )?;

    let Selection::Choice(login_choice) = login_choice;

    match login_choice + 1 {
        1 => {
            config.onboarded = Some(true);
            config.save()?;
            match auth::login_with_browser(api_base, model, &config, false).await {
                Ok(mut signed_in) => {
                    signed_in.onboarded = Some(true);
                    if signed_in.theme.is_none() {
                        signed_in.theme = config.theme;
                    }
                    signed_in.save()?;
                    clear_screen();
                    Ok(signed_in)
                }
                Err(_) => {
                    clear_screen();
                    println!(
                        "{}",
                        terminal.muted(
                            "Run `codeus` again when you are ready to connect your Quantum Club account."
                        )
                    );
                    process::exit(0);
                }
            }
        }
        _ => unreachable!("read_choice clamps the selected range"),
    }
}

pub fn choose_appearance(config: &mut Config) -> Result<()> {
    choose_theme(config)?;
    Ok(())
}

pub fn choose_theme(config: &mut Config) -> Result<()> {
    let current = config.theme.as_deref().unwrap_or("auto");
    let current_index = THEME_CHOICES
        .iter()
        .position(|choice| choice.key == current)
        .unwrap_or(0);
    let labels: Vec<&str> = THEME_CHOICES.iter().map(|choice| choice.label).collect();

    let selection = select_index(
        &Terminal::detect_with_theme(Some(current)),
        "Choose the text style that looks best with your terminal",
        "To change this later, run /theme",
        &labels,
        current_index,
        SelectorStyle::Theme,
        |index| {
            let preview_terminal = Terminal::detect_with_theme(Some(THEME_CHOICES[index].key));
            print_theme_sample(&preview_terminal)
        },
    )?;
    let Selection::Choice(selected) = selection;
    let selected = selected + 1;
    let choice = THEME_CHOICES
        .get(selected - 1)
        .context("theme selection was out of range")?;
    config.theme = Some(choice.key.to_string());
    config.save()?;

    let label = theme_label(choice.key).unwrap_or(choice.label);
    let terminal = Terminal::detect_with_theme(Some(choice.key));
    println!("{}", terminal.success(&format!("Text style: {label}")));
    Ok(())
}

fn print_theme_sample(terminal: &Terminal) -> String {
    [
        terminal.muted("----------------------------------------"),
        format!(
            "  {}  {} {}() {{",
            terminal.muted("1"),
            terminal.sample_keyword("function"),
            terminal.sample_function("greet")
        ),
        format!(
            "  {}    {}",
            terminal.muted("2"),
            terminal.sample_comment("// syntax preview follows this theme")
        ),
        format!(
            "  {}    {} message = {};",
            terminal.muted("3"),
            terminal.sample_keyword("const"),
            terminal.sample_string("\"Hello, Codeus!\"")
        ),
        format!(
            "  {}{}",
            terminal.muted("4"),
            terminal.sample_deletion(" -  console.log(\"Hello, World!\");")
        ),
        format!(
            "  {}{}",
            terminal.muted("4"),
            terminal.sample_addition(" +  console.log(message);")
        ),
        format!("  {}  }}", terminal.muted("5")),
        terminal.muted("----------------------------------------"),
    ]
    .join("\n")
}

#[derive(Debug, Clone, Copy)]
enum SelectorStyle {
    Theme,
    Login,
}

enum Selection {
    Choice(usize),
}

fn select_index<F>(
    terminal: &Terminal,
    title: &str,
    hint: &str,
    choices: &[&str],
    default: usize,
    style: SelectorStyle,
    preview: F,
) -> Result<Selection>
where
    F: Fn(usize) -> String,
{
    if choices.is_empty() {
        bail!("selector requires at least one choice");
    }
    if default >= choices.len() {
        bail!("default selection must be in range");
    }

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return read_choice(
            &format!("Enter 1-{}", choices.len()),
            choices.len(),
            default + 1,
        )
        .map(|choice| Selection::Choice(choice - 1));
    }

    interactive_select(terminal, title, hint, choices, default, style, preview)
}

#[cfg(unix)]
fn render_selector(
    terminal: &Terminal,
    title: &str,
    hint: &str,
    choices: &[&str],
    selected: usize,
    style: SelectorStyle,
    preview: &str,
) -> String {
    let selected_terminal;
    let terminal = match style {
        SelectorStyle::Login => terminal,
        SelectorStyle::Theme => {
            selected_terminal = Terminal::detect_with_theme(Some(THEME_CHOICES[selected].key));
            &selected_terminal
        }
    };

    let mut lines = Vec::new();
    if matches!(style, SelectorStyle::Theme | SelectorStyle::Login) {
        lines.push(String::new());
        lines.extend(terminal.welcome_art_lines());
        lines.push(String::new());
    }
    if matches!(style, SelectorStyle::Theme) {
        lines.push("Let's get started.".to_string());
        lines.push(String::new());
    }
    lines.push(terminal.title(title));
    if !hint.is_empty() {
        lines.push(terminal.muted(hint));
    }
    lines.push(String::new());

    for (index, choice) in choices.iter().enumerate() {
        if index == selected {
            lines.push(format!(
                "{} {} {}",
                terminal.selection(">"),
                terminal.muted(&format!("{}.", index + 1)),
                terminal.selection(choice)
            ));
        } else {
            lines.push(format!(
                "  {} {}",
                terminal.muted(&format!("{}.", index + 1)),
                choice
            ));
        }
    }

    if !preview.is_empty() {
        lines.push(String::new());
        lines.push(preview.to_string());
        if matches!(style, SelectorStyle::Theme) {
            let label =
                theme_label(THEME_CHOICES[selected].key).unwrap_or(THEME_CHOICES[selected].label);
            lines.push(terminal.muted(&format!("Syntax theme: {label} (ctrl+t to disable)")));
        }
    }

    lines.join("\n")
}

fn clear_screen() {
    if io::stdout().is_terminal() {
        print!("\x1b[2J\x1b[H");
        let _ = io::stdout().flush();
    }
}

fn interactive_select<F>(
    terminal: &Terminal,
    title: &str,
    hint: &str,
    choices: &[&str],
    default: usize,
    style: SelectorStyle,
    preview: F,
) -> Result<Selection>
where
    F: Fn(usize) -> String,
{
    #[cfg(unix)]
    {
        let _raw = RawMode::enable()?;
        let _cursor = HiddenCursor::hide();
        let mut selected = default;
        let mut needs_render = true;
        clear_screen();
        print!("\x1b7");
        io::stdout().flush()?;

        loop {
            if needs_render {
                let preview_text = preview(selected);
                let preview = &preview_text;
                let output =
                    render_selector(terminal, title, hint, choices, selected, style, preview);

                print!("\x1b8\x1b[J");
                print!("{output}");
                io::stdout().flush()?;
                needs_render = false;
            }

            match read_key()? {
                Key::Up | Key::Left => {
                    selected = selected.checked_sub(1).unwrap_or(choices.len() - 1);
                    needs_render = true;
                }
                Key::Down | Key::Right | Key::Tab => {
                    selected = (selected + 1) % choices.len();
                    needs_render = true;
                }
                Key::ToggleTheme if matches!(style, SelectorStyle::Theme) => {
                    let disabled = THEME_CHOICES
                        .iter()
                        .position(|choice| choice.key == "none")
                        .unwrap_or(choices.len() - 1);
                    selected = if selected == disabled {
                        default.min(choices.len() - 1)
                    } else {
                        disabled
                    };
                    needs_render = true;
                }
                Key::Enter => {
                    println!();
                    return Ok(Selection::Choice(selected));
                }
                Key::Digit(value) if (1..=choices.len()).contains(&value) => {
                    let next = value - 1;
                    if next != selected {
                        selected = next;
                        needs_render = true;
                    }
                }
                Key::Digit(_) => {}
                Key::Char('k' | 'K') => {
                    selected = selected.checked_sub(1).unwrap_or(choices.len() - 1);
                    needs_render = true;
                }
                Key::Char('j' | 'J') => {
                    selected = (selected + 1) % choices.len();
                    needs_render = true;
                }
                Key::Escape => {
                    println!();
                    return Ok(Selection::Choice(default));
                }
                Key::Interrupt => {
                    println!();
                    bail!("selection cancelled");
                }
                Key::ToggleTheme | Key::Backspace | Key::Char(_) | Key::Other => {}
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = (terminal, title, hint, style, &preview);
        read_choice(
            &format!("Enter 1-{}", choices.len()),
            choices.len(),
            default + 1,
        )
        .map(|choice| Selection::Choice(choice - 1))
    }
}

#[derive(Debug, Clone, Copy)]
#[cfg(unix)]
enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Escape,
    Interrupt,
    Tab,
    ToggleTheme,
    Digit(usize),
    Backspace,
    Char(char),
    Other,
}

#[cfg(unix)]
fn read_key() -> Result<Key> {
    let mut stdin = io::stdin().lock();
    let byte = read_byte_blocking(&mut stdin)?;

    match byte {
        b'\n' | b'\r' => Ok(Key::Enter),
        b'\t' => Ok(Key::Tab),
        0x03 => Ok(Key::Interrupt),
        0x14 => Ok(Key::ToggleTheme),
        0x7f | 0x08 => Ok(Key::Backspace),
        b'1'..=b'9' => Ok(Key::Digit((byte - b'0') as usize)),
        0x1b => {
            let sequence = read_escape_sequence(&mut stdin)?;
            if sequence.is_empty() {
                return Ok(Key::Escape);
            }

            match sequence.last().copied() {
                Some(b'A') => Ok(Key::Up),
                Some(b'B') => Ok(Key::Down),
                Some(b'C') => Ok(Key::Right),
                Some(b'D') => Ok(Key::Left),
                _ => Ok(Key::Other),
            }
        }
        byte if byte.is_ascii_graphic() || byte == b' ' => Ok(Key::Char(byte as char)),
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
fn read_byte_blocking<R: Read>(reader: &mut R) -> Result<u8> {
    loop {
        if let Some(byte) = read_byte_timeout(reader)? {
            return Ok(byte);
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

#[cfg(unix)]
struct HiddenCursor;

#[cfg(unix)]
impl HiddenCursor {
    fn hide() -> Self {
        if io::stdout().is_terminal() {
            print!("\x1b[?25l");
            let _ = io::stdout().flush();
        }
        Self
    }
}

#[cfg(unix)]
impl Drop for HiddenCursor {
    fn drop(&mut self) {
        if io::stdout().is_terminal() {
            print!("\x1b[?25h");
            let _ = io::stdout().flush();
        }
    }
}

fn read_choice(prompt: &str, max: usize, default: usize) -> Result<usize> {
    if default == 0 || default > max {
        bail!("default selection must be between 1 and {max}");
    }

    loop {
        print!("{prompt} [{default}] > ");
        io::stdout().flush()?;

        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 {
            bail!("input closed before a selection was made");
        }

        let input = input.trim();
        if input.is_empty() {
            return Ok(default);
        }

        match input.parse::<usize>() {
            Ok(value) if (1..=max).contains(&value) => return Ok(value),
            _ => println!("Choose a number from 1 to {max}."),
        }
    }
}
