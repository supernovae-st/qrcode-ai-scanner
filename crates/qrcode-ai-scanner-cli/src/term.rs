//! The colour seam — the ONLY home for raw ANSI in the `qrscan`
//! binary (one seam per binary · semantic-not-decorative · gate
//! `NO_COLOR > CLICOLOR_FORCE > TTY`). JSON and `--score-only` are
//! machine surfaces and never route through here.

use std::io::IsTerminal;

/// The gate, pure — unit-testable without a terminal.
#[must_use]
pub fn resolve_colour(no_color: bool, clicolor_force: bool, is_tty: bool) -> bool {
    if no_color {
        return false;
    }
    if clicolor_force {
        return true;
    }
    is_tty
}

/// Auto-resetting semantic theme; every helper closes its own colour.
#[derive(Clone, Copy)]
pub struct Theme {
    on: bool,
}

impl Theme {
    /// Gate on STDOUT (the pretty surface).
    #[must_use]
    pub fn auto() -> Self {
        Self {
            on: resolve_colour(
                std::env::var_os("NO_COLOR").is_some(),
                std::env::var_os("CLICOLOR_FORCE").is_some(),
                std::io::stdout().is_terminal(),
            ),
        }
    }

    /// Gate on STDERR (the error surface).
    #[must_use]
    pub fn auto_stderr() -> Self {
        Self {
            on: resolve_colour(
                std::env::var_os("NO_COLOR").is_some(),
                std::env::var_os("CLICOLOR_FORCE").is_some(),
                std::io::stderr().is_terminal(),
            ),
        }
    }

    fn wrap(self, code: &str, s: &str) -> String {
        if self.on {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    #[must_use]
    pub fn ok(self, s: &str) -> String {
        self.wrap("32", s)
    }
    #[must_use]
    pub fn warn(self, s: &str) -> String {
        self.wrap("33", s)
    }
    #[must_use]
    pub fn dim(self, s: &str) -> String {
        self.wrap("2", s)
    }
    #[must_use]
    pub fn ok_strong(self, s: &str) -> String {
        self.wrap("1;32", s)
    }
    #[must_use]
    pub fn err_strong(self, s: &str) -> String {
        self.wrap("1;31", s)
    }

    /// Colour text by the scanner's own grade vocabulary — the grade
    /// IS the semantic, the colour just makes it pre-attentive.
    #[must_use]
    pub fn grade_text(self, grade: &str, text: &str) -> String {
        match grade {
            "Excellent" => self.ok_strong(text),
            "Good" => self.ok(text),
            "Fair" => self.warn(text),
            _ => self.err_strong(text),
        }
    }
}

/// clap help styling — one `styles =` on the root colours every
/// `--help` (clap gates itself on TTY + `NO_COLOR`).
#[must_use]
pub fn clap_styles() -> clap::builder::Styles {
    use clap::builder::styling::{AnsiColor, Style};
    clap::builder::Styles::styled()
        .header(Style::new().bold().fg_color(Some(AnsiColor::Cyan.into())))
        .usage(Style::new().bold().fg_color(Some(AnsiColor::Cyan.into())))
        .literal(Style::new().fg_color(Some(AnsiColor::Green.into())))
        .placeholder(Style::new().fg_color(Some(AnsiColor::Cyan.into())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_precedence_matches_the_house_contract() {
        assert!(!resolve_colour(true, true, true));
        assert!(resolve_colour(false, true, false));
        assert!(resolve_colour(false, false, true));
        assert!(!resolve_colour(false, false, false));
    }

    #[test]
    fn theme_off_is_byte_transparent() {
        let t = Theme { on: false };
        assert_eq!(t.grade_text("Excellent", "97/100"), "97/100");
        assert_eq!(t.err_strong("x"), "x");
    }

    #[test]
    fn grades_speak_their_colours() {
        let t = Theme { on: true };
        assert!(t.grade_text("Excellent", "x").contains("1;32"));
        assert!(t.grade_text("Good", "x").contains("[32m"));
        assert!(t.grade_text("Fair", "x").contains("[33m"));
        assert!(t.grade_text("Poor", "x").contains("1;31"));
    }
}
