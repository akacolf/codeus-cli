use crate::auth;
use crate::config::Config;
use crate::terminal::{font_label, theme_label, Terminal, FONT_CHOICES, THEME_CHOICES};
use anyhow::{bail, Context, Result};
use std::io::{self, IsTerminal, Read, Write};

pub fn should_run(config: &Config) -> bool {
    config.session_token.is_none() && io::stdin().is_terminal() && io::stdout().is_terminal()
}

pub async fn run(
    api_base: Option<String>,
    model: Option<String>,
    mut config: Config,
) -> Result<Config> {
    choose_theme(&mut config)?;
    choose_font(&mut config)?;
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
                    if signed_in.font.is_none() {
                        signed_in.font = config.font;
                    }
                    signed_in.save()?;
                    clear_screen();
                    Ok(signed_in)
                }
                Err(error) => {
                    println!(
                        "{} Browser login could not finish: {error}",
                        terminal.warn("•")
                    );
                    println!(
                        "{}",
                        terminal.muted(
                            "You can run `codeus login` later, or `codeus login --token ...` for development/support."
                        )
                    );
                    Ok(Config::load()?)
                }
            }
        }
        _ => unreachable!("read_choice clamps the selected range"),
    }
}

pub fn choose_appearance(config: &mut Config) -> Result<()> {
    choose_theme(config)?;
    choose_font(config)?;
    Ok(())
}

pub fn choose_theme(config: &mut Config) -> Result<()> {
    let current = config.theme.as_deref().unwrap_or("auto");
    let current_index = THEME_CHOICES
        .iter()
        .position(|choice| choice.key == current)
        .unwrap_or(0);
    let labels: Vec<&str> = THEME_CHOICES.iter().map(|choice| choice.label).collect();

    let selected = select_index(
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
    )? + 1;
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

pub fn choose_font(config: &mut Config) -> Result<()> {
    let current = config.font.as_deref().unwrap_or("auto");
    let custom_index = FONT_CHOICES.len() - 1;
    let current_index = FONT_CHOICES
        .iter()
        .position(|choice| choice.key == current)
        .unwrap_or(custom_index);
    let labels: Vec<&str> = FONT_CHOICES.iter().map(|choice| choice.label).collect();

    let selected = select_index(
        &Terminal::detect_with_theme(config.theme.as_deref()),
        "Choose your coding font preference",
        "To change this later, run /font",
        &labels,
        current_index,
        SelectorStyle::Font,
        |index| print_font_sample(FONT_CHOICES[index].label),
    )?;

    let choice = FONT_CHOICES
        .get(selected)
        .context("font selection was out of range")?;
    config.font = if choice.key == "custom" {
        Some(read_custom_font(current)?)
    } else {
        Some(choice.key.to_string())
    };
    config.save()?;

    let terminal = Terminal::detect_with_theme(config.theme.as_deref());
    let label = font_label(config.font.as_deref());
    println!("{}", terminal.success(&format!("Font preference: {label}")));
    println!(
        "{}",
        terminal.muted("Terminal apps control the actual font; Codeus stores the preference.")
    );
    Ok(())
}

fn print_theme_sample(terminal: &Terminal) -> String {
    [
        terminal.muted("----------------------------------------"),
        format!(
            "  1  {} {}() {{",
            terminal.sample_keyword("function"),
            terminal.sample_function("greet")
        ),
        format!(
            "  2    {}",
            terminal.sample_comment("// syntax preview follows this theme")
        ),
        format!(
            "  3    {} message = {};",
            terminal.sample_keyword("const"),
            terminal.sample_string("\"Hello, Codeus!\"")
        ),
        terminal.sample_deletion("  4 -  console.log(\"Hello, World!\");"),
        terminal.sample_addition("  4 +  console.log(message);"),
        "  5  }".to_string(),
        terminal.muted("----------------------------------------"),
    ]
    .join("\n")
}

fn print_font_sample(label: &str) -> String {
    [
        "----------------------------------------".to_string(),
        format!("  Font: {label}"),
        "  abcdefghijklmnopqrstuvwxyz".to_string(),
        "  ABCDEFGHIJKLMNOPQRSTUVWXYZ".to_string(),
        "  0123456789  {} [] () <> /\\ |".to_string(),
        "  fn main() { println!(\"Codeus\"); }".to_string(),
        "----------------------------------------".to_string(),
    ]
    .join("\n")
}

fn read_custom_font(current: &str) -> Result<String> {
    let fallback = if FONT_CHOICES.iter().any(|choice| choice.key == current) {
        "JetBrains Mono"
    } else {
        current
    };

    print!("Custom font name [{fallback}] > ");
    io::stdout().flush()?;

    let mut input = String::new();
    if io::stdin().read_line(&mut input)? == 0 {
        bail!("input closed before a custom font was entered");
    }

    let input = input.trim();
    if input.is_empty() {
        Ok(fallback.to_string())
    } else {
        Ok(input.to_string())
    }
}

#[derive(Debug, Clone, Copy)]
enum SelectorStyle {
    Theme,
    Font,
    Login,
}

fn select_index<F>(
    terminal: &Terminal,
    title: &str,
    hint: &str,
    choices: &[&str],
    default: usize,
    style: SelectorStyle,
    preview: F,
) -> Result<usize>
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
        .map(|choice| choice - 1);
    }

    interactive_select(terminal, title, hint, choices, default, style, preview)
}

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
        SelectorStyle::Font | SelectorStyle::Login => terminal,
        SelectorStyle::Theme => {
            selected_terminal = Terminal::detect_with_theme(Some(THEME_CHOICES[selected].key));
            &selected_terminal
        }
    };

    let mut lines = Vec::new();
    if matches!(
        style,
        SelectorStyle::Theme | SelectorStyle::Font | SelectorStyle::Login
    ) {
        lines.push(String::new());
        lines.extend(terminal.welcome_art_lines());
        lines.push(String::new());
    }
    if matches!(style, SelectorStyle::Theme | SelectorStyle::Font) {
        lines.push("Let's get started.".to_string());
        lines.push(String::new());
    }
    lines.push(terminal.title(title));
    if !hint.is_empty() {
        lines.push(terminal.muted(hint));
    }
    lines.push(String::new());

    for (index, choice) in choices.iter().enumerate() {
        let line = format!(
            "{} {}. {choice}",
            if index == selected { ">" } else { " " },
            index + 1
        );
        if index == selected {
            lines.push(terminal.selection(&line));
        } else {
            lines.push(line);
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
) -> Result<usize>
where
    F: Fn(usize) -> String,
{
    #[cfg(unix)]
    {
        let _raw = RawMode::enable()?;
        let mut selected = default;
        let mut needs_render = true;
        clear_screen();
        print!("\x1b7");
        io::stdout().flush()?;

        loop {
            if needs_render {
                let output = render_selector(
                    terminal,
                    title,
                    hint,
                    choices,
                    selected,
                    style,
                    &preview(selected),
                );

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
                    return Ok(selected);
                }
                Key::Digit(value) if (1..=choices.len()).contains(&value) => {
                    let next = value - 1;
                    if next != selected {
                        selected = next;
                        needs_render = true;
                    }
                }
                Key::Digit(_) => {}
                Key::Escape => {
                    println!();
                    return Ok(default);
                }
                Key::Interrupt => {
                    println!();
                    bail!("selection cancelled");
                }
                Key::ToggleTheme | Key::Other => {}
            }
        }
    }

    #[cfg(not(unix))]
    {
        read_choice(
            &format!("Enter 1-{}", choices.len()),
            choices.len(),
            default + 1,
        )
        .map(|choice| choice - 1)
    }
}

#[derive(Debug, Clone, Copy)]
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
        b'1'..=b'9' => Ok(Key::Digit((byte - b'0') as usize)),
        b'j' | b'J' => Ok(Key::Down),
        b'k' | b'K' => Ok(Key::Up),
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
