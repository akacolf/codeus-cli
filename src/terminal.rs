use std::io::{self, IsTerminal};

#[derive(Debug, Clone, Copy)]
pub struct ThemeChoice {
    pub key: &'static str,
    pub label: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct FontChoice {
    pub key: &'static str,
    pub label: &'static str,
}

pub const THEME_CHOICES: &[ThemeChoice] = &[
    ThemeChoice {
        key: "auto",
        label: "Auto (Monokai Extended)",
    },
    ThemeChoice {
        key: "monokai",
        label: "Monokai Extended",
    },
    ThemeChoice {
        key: "monokai-light",
        label: "Monokai Extended Light",
    },
    ThemeChoice {
        key: "github-dark",
        label: "GitHub Dark",
    },
    ThemeChoice {
        key: "github-light",
        label: "GitHub Light",
    },
    ThemeChoice {
        key: "solarized-dark",
        label: "Solarized Dark",
    },
    ThemeChoice {
        key: "solarized-light",
        label: "Solarized Light",
    },
    ThemeChoice {
        key: "ansi",
        label: "ANSI colors only",
    },
];

pub const FONT_CHOICES: &[FontChoice] = &[
    FontChoice {
        key: "auto",
        label: "Terminal default",
    },
    FontChoice {
        key: "jetbrains-mono",
        label: "JetBrains Mono",
    },
    FontChoice {
        key: "fira-code",
        label: "Fira Code",
    },
    FontChoice {
        key: "cascadia-code",
        label: "Cascadia Code",
    },
    FontChoice {
        key: "sf-mono",
        label: "SF Mono",
    },
    FontChoice {
        key: "menlo",
        label: "Menlo",
    },
    FontChoice {
        key: "monaco",
        label: "Monaco",
    },
    FontChoice {
        key: "consolas",
        label: "Consolas",
    },
    FontChoice {
        key: "source-code-pro",
        label: "Source Code Pro",
    },
    FontChoice {
        key: "ibm-plex-mono",
        label: "IBM Plex Mono",
    },
    FontChoice {
        key: "inconsolata",
        label: "Inconsolata",
    },
    FontChoice {
        key: "ubuntu-mono",
        label: "Ubuntu Mono",
    },
    FontChoice {
        key: "dejavu-sans-mono",
        label: "DejaVu Sans Mono",
    },
    FontChoice {
        key: "custom",
        label: "Custom font name...",
    },
];

pub struct Terminal {
    color: bool,
    palette: Palette,
}

#[derive(Debug, Clone, Copy)]
enum Palette {
    Monokai,
    MonokaiLight,
    GitHubDark,
    GitHubLight,
    SolarizedDark,
    SolarizedLight,
    Ansi,
}

impl Terminal {
    pub fn detect_with_theme(theme: Option<&str>) -> Self {
        let color = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        let palette = match theme.unwrap_or("auto") {
            "monokai-light" | "light" | "light-colorblind" => Palette::MonokaiLight,
            "github-dark" => Palette::GitHubDark,
            "github-light" => Palette::GitHubLight,
            "solarized-dark" | "dark-colorblind" => Palette::SolarizedDark,
            "solarized-light" => Palette::SolarizedLight,
            "ansi" | "dark-ansi" | "light-ansi" => Palette::Ansi,
            _ => Palette::Monokai,
        };

        Self { color, palette }
    }

    pub fn logo(&self) -> String {
        let raw = [
            "      +----------------+",
            "      |                |",
            "      |     < / >      |",
            "      |                |",
            "      +----------------+",
        ]
        .join("\n");

        self.accent(&raw)
    }

    pub fn accent(&self, value: &str) -> String {
        if self.color {
            format!("{}{value}\x1b[0m", self.accent_code())
        } else {
            value.to_string()
        }
    }

    pub fn title(&self, value: &str) -> String {
        if self.color {
            format!("{}{value}\x1b[0m", self.title_code())
        } else {
            value.to_string()
        }
    }

    pub fn muted(&self, value: &str) -> String {
        if self.color {
            format!("\x1b[2m{value}\x1b[0m")
        } else {
            value.to_string()
        }
    }

    pub fn success(&self, value: &str) -> String {
        if self.color {
            format!("{}{value}\x1b[0m", self.success_code())
        } else {
            value.to_string()
        }
    }

    pub fn warn(&self, value: &str) -> String {
        if self.color {
            format!("{}{value}\x1b[0m", self.warn_code())
        } else {
            value.to_string()
        }
    }

    pub fn selection(&self, value: &str) -> String {
        if self.color {
            format!("\x1b[1m{}{value}\x1b[0m", self.accent_code())
        } else {
            value.to_string()
        }
    }

    pub fn sample_keyword(&self, value: &str) -> String {
        self.fg(value, self.colors().keyword)
    }

    pub fn sample_function(&self, value: &str) -> String {
        self.fg(value, self.colors().function)
    }

    pub fn sample_string(&self, value: &str) -> String {
        self.fg(value, self.colors().string)
    }

    pub fn sample_comment(&self, value: &str) -> String {
        self.fg(value, self.colors().comment)
    }

    pub fn sample_addition(&self, value: &str) -> String {
        let colors = self.colors();
        self.fg_bg(value, colors.diff_fg, colors.diff_add_bg)
    }

    pub fn sample_deletion(&self, value: &str) -> String {
        let colors = self.colors();
        self.fg_bg(value, colors.diff_fg, colors.diff_del_bg)
    }

    fn title_code(&self) -> &'static str {
        "\x1b[1m"
    }

    fn accent_code(&self) -> &'static str {
        match self.palette {
            Palette::Monokai => "\x1b[38;2;102;217;239m",
            Palette::MonokaiLight => "\x1b[38;2;111;66;193m",
            Palette::GitHubDark => "\x1b[38;2;121;192;255m",
            Palette::GitHubLight => "\x1b[38;2;9;105;218m",
            Palette::SolarizedDark => "\x1b[38;2;38;139;210m",
            Palette::SolarizedLight => "\x1b[38;2;38;139;210m",
            Palette::Ansi => "\x1b[36m",
        }
    }

    fn success_code(&self) -> &'static str {
        match self.palette {
            Palette::GitHubDark => "\x1b[38;2;63;185;80m",
            Palette::GitHubLight => "\x1b[38;2;26;127;55m",
            Palette::SolarizedDark | Palette::SolarizedLight => "\x1b[38;2;133;153;0m",
            Palette::Ansi => "\x1b[32m",
            _ => "\x1b[38;2;166;226;46m",
        }
    }

    fn warn_code(&self) -> &'static str {
        match self.palette {
            Palette::GitHubDark => "\x1b[38;2;210;153;34m",
            Palette::GitHubLight => "\x1b[38;2;154;103;0m",
            Palette::SolarizedDark | Palette::SolarizedLight => "\x1b[38;2;181;54;130m",
            Palette::Ansi => "\x1b[33m",
            _ => "\x1b[38;2;230;219;116m",
        }
    }

    fn fg(&self, value: &str, color: Color) -> String {
        if self.color {
            format!("{}{value}\x1b[0m", color.fg_code())
        } else {
            value.to_string()
        }
    }

    fn fg_bg(&self, value: &str, fg: Color, bg: Color) -> String {
        if self.color {
            format!("{}{}{value}\x1b[0m", fg.fg_code(), bg.bg_code())
        } else {
            value.to_string()
        }
    }

    fn colors(&self) -> ThemeColors {
        match self.palette {
            Palette::Monokai => ThemeColors {
                keyword: Color::Rgb(249, 38, 114),
                function: Color::Rgb(166, 226, 46),
                string: Color::Rgb(230, 219, 116),
                comment: Color::Rgb(117, 113, 94),
                diff_fg: Color::Rgb(248, 248, 242),
                diff_add_bg: Color::Rgb(39, 89, 45),
                diff_del_bg: Color::Rgb(92, 28, 46),
            },
            Palette::MonokaiLight => ThemeColors {
                keyword: Color::Rgb(111, 66, 193),
                function: Color::Rgb(80, 161, 79),
                string: Color::Rgb(152, 104, 1),
                comment: Color::Rgb(160, 161, 167),
                diff_fg: Color::Rgb(36, 41, 47),
                diff_add_bg: Color::Rgb(198, 246, 213),
                diff_del_bg: Color::Rgb(255, 205, 210),
            },
            Palette::GitHubDark => ThemeColors {
                keyword: Color::Rgb(255, 123, 114),
                function: Color::Rgb(210, 168, 255),
                string: Color::Rgb(165, 214, 255),
                comment: Color::Rgb(139, 148, 158),
                diff_fg: Color::Rgb(201, 209, 217),
                diff_add_bg: Color::Rgb(46, 125, 50),
                diff_del_bg: Color::Rgb(139, 38, 53),
            },
            Palette::GitHubLight => ThemeColors {
                keyword: Color::Rgb(207, 34, 46),
                function: Color::Rgb(111, 66, 193),
                string: Color::Rgb(10, 48, 105),
                comment: Color::Rgb(87, 96, 106),
                diff_fg: Color::Rgb(36, 41, 47),
                diff_add_bg: Color::Rgb(218, 251, 225),
                diff_del_bg: Color::Rgb(255, 235, 233),
            },
            Palette::SolarizedDark => ThemeColors {
                keyword: Color::Rgb(211, 54, 130),
                function: Color::Rgb(38, 139, 210),
                string: Color::Rgb(42, 161, 152),
                comment: Color::Rgb(101, 123, 131),
                diff_fg: Color::Rgb(238, 232, 213),
                diff_add_bg: Color::Rgb(64, 91, 48),
                diff_del_bg: Color::Rgb(101, 39, 44),
            },
            Palette::SolarizedLight => ThemeColors {
                keyword: Color::Rgb(211, 54, 130),
                function: Color::Rgb(38, 139, 210),
                string: Color::Rgb(42, 161, 152),
                comment: Color::Rgb(147, 161, 161),
                diff_fg: Color::Rgb(88, 110, 117),
                diff_add_bg: Color::Rgb(220, 236, 210),
                diff_del_bg: Color::Rgb(244, 213, 216),
            },
            Palette::Ansi => ThemeColors {
                keyword: Color::Ansi(35),
                function: Color::Ansi(32),
                string: Color::Ansi(33),
                comment: Color::Ansi(90),
                diff_fg: Color::Ansi(30),
                diff_add_bg: Color::AnsiBg(42),
                diff_del_bg: Color::AnsiBg(41),
            },
        }
    }
}

pub fn theme_label(key: &str) -> Option<&'static str> {
    THEME_CHOICES
        .iter()
        .find(|choice| choice.key == key)
        .map(|choice| choice.label)
}

pub fn font_label(font: Option<&str>) -> String {
    let key = font.unwrap_or("auto");
    FONT_CHOICES
        .iter()
        .find(|choice| choice.key == key)
        .map(|choice| choice.label.to_string())
        .unwrap_or_else(|| key.to_string())
}

#[derive(Debug, Clone, Copy)]
struct ThemeColors {
    keyword: Color,
    function: Color,
    string: Color,
    comment: Color,
    diff_fg: Color,
    diff_add_bg: Color,
    diff_del_bg: Color,
}

#[derive(Debug, Clone, Copy)]
enum Color {
    Rgb(u8, u8, u8),
    Ansi(u8),
    AnsiBg(u8),
}

impl Color {
    fn fg_code(self) -> String {
        match self {
            Self::Rgb(red, green, blue) => format!("\x1b[38;2;{red};{green};{blue}m"),
            Self::Ansi(code) | Self::AnsiBg(code) => format!("\x1b[{code}m"),
        }
    }

    fn bg_code(self) -> String {
        match self {
            Self::Rgb(red, green, blue) => format!("\x1b[48;2;{red};{green};{blue}m"),
            Self::Ansi(code) => format!("\x1b[{code}m"),
            Self::AnsiBg(code) => format!("\x1b[{code}m"),
        }
    }
}
