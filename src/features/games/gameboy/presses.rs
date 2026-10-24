//! What to press, typed as text: `a a up*3 a+b wait start`. Pure, no emulator.
//!
//! - a button name (`a`, `b`, `start`, `select`, `up`, `down`, `left`, `right`, or `u`,
//!   `d`, `l`, `r`) is one press
//! - `a+b` presses both at once
//! - `:40` after either holds it 40 frames, not the usual length (a charged shot, a higher
//!   jump)
//! - `a:20+right:45` holds each button its own frames, all pressed at once: a button
//!   without a number is held as long as the longest
//! - `*3` after any of them repeats it
//! - `wait` presses nothing for as long as a press takes
//!
//! Spaces or commas separate presses.

use super::emulator::Button;

/// The most presses in one go, so one command can't run for minutes.
pub const MOST: usize = 64;
/// The longest one press can be held, in frames (two seconds).
pub const LONGEST: u32 = 120;

/// One press: these buttons held together, then let go. No buttons is a wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Press {
    pub buttons: Vec<Button>,
    /// Frames to hold it, when not the usual length.
    pub frames: Option<u32>,
    /// Buttons let go before the rest, and after how many frames.
    pub shorter: Vec<(Button, u32)>,
}

impl Press {
    /// These buttons for the usual length.
    pub fn of(buttons: Vec<Button>) -> Press {
        Press {
            buttons,
            frames: None,
            shorter: Vec::new(),
        }
    }

    /// Frames `button` is held, when a press usually lasts `usual` frames.
    pub fn frames_of(&self, button: Button, usual: u32) -> u32 {
        let whole = self.frames.unwrap_or(usual);
        match self.shorter.iter().find(|(b, _)| *b == button) {
            Some(&(_, frames)) => frames.min(whole),
            None => whole,
        }
    }
}

/// Reads a list of presses, or says what's wrong with it in words for the person who
/// typed it.
pub fn parse(text: &str) -> Result<Vec<Press>, String> {
    let mut presses = Vec::new();
    for word in text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty())
    {
        let (what, times) = match word.split_once(['*', 'x']) {
            // `x` only counts as "times" when a number follows, so `x` alone stays a
            // (wrong) button name and gets the usual message.
            Some((what, times)) if !what.is_empty() => {
                let times: usize = times
                    .parse()
                    .ok()
                    .filter(|&n| n >= 1)
                    .ok_or_else(|| format!("`{word}`: put a number from 1 up after `*`."))?;
                (what, times)
            }
            _ => (word, 1),
        };
        let mut parts = Vec::new();
        for part in what.split('+') {
            let (name, frames) = match part.split_once(':') {
                Some((name, frames)) => {
                    let frames: u32 = frames
                        .parse()
                        .ok()
                        .filter(|n| (1..=LONGEST).contains(n))
                        .ok_or_else(|| {
                            format!("`{word}`: put frames from 1 to {LONGEST} after `:`.")
                        })?;
                    (name, Some(frames))
                }
                None => (part, None),
            };
            parts.push((name, frames));
        }
        // The press lasts as long as its longest button.
        let frames = parts.iter().filter_map(|&(_, f)| f).max();
        let press = if let [(name, _)] = parts.as_slice()
            && name.eq_ignore_ascii_case("wait")
        {
            Press {
                buttons: Vec::new(),
                frames,
                shorter: Vec::new(),
            }
        } else {
            let mut buttons = Vec::new();
            let mut shorter = Vec::new();
            for (name, own) in parts {
                let button = Button::parse(name).ok_or_else(|| {
                    // `aa` for two presses of A, written together.
                    let letters =
                        name.len() > 1 && name.chars().all(|c| "abudlrABUDLR".contains(c));
                    if letters {
                        let apart: Vec<String> = name.chars().map(String::from).collect();
                        format!(
                            "`{name}` isn't a button: put spaces between presses, like `{}`.",
                            apart.join(" ")
                        )
                    } else {
                        format!(
                            "`{name}` isn't a button. The buttons are a, b, start, select, up, \
                             down, left and right."
                        )
                    }
                })?;
                if !buttons.contains(&button) {
                    buttons.push(button);
                    if let Some(own) = own.filter(|&own| Some(own) < frames) {
                        shorter.push((button, own));
                    }
                }
            }
            Press {
                buttons,
                frames,
                shorter,
            }
        };
        if presses.len() + times > MOST {
            return Err(format!(
                "That's more than {MOST} presses. Send them in parts."
            ));
        }
        presses.extend(std::iter::repeat_n(press, times));
    }
    if presses.is_empty() {
        return Err("Say which buttons to press, like `a a up*3 start`.".to_string());
    }
    Ok(presses)
}

/// The presses written back the way `parse` reads them, with repeats folded:
/// `a up*3 a+b b:40`.
pub fn describe(presses: &[Press]) -> String {
    let name = |press: &Press| {
        let mut names: Vec<String> = press
            .buttons
            .iter()
            .map(|&b| match press.shorter.iter().find(|(s, _)| *s == b) {
                Some((_, frames)) => format!("{}:{frames}", b.name()),
                None => b.name().to_string(),
            })
            .collect();
        if names.is_empty() {
            names.push("wait".to_string());
        }
        // The press's length goes after the last button held all of it.
        if let Some(frames) = press.frames
            && let Some(last) = names.iter_mut().rev().find(|n| !n.contains(':'))
        {
            last.push_str(&format!(":{frames}"));
        }
        names.join("+")
    };
    let mut words: Vec<String> = Vec::new();
    let mut i = 0;
    while i < presses.len() {
        let mut run = 1;
        while i + run < presses.len() && presses[i + run] == presses[i] {
            run += 1;
        }
        words.push(match run {
            1 => name(&presses[i]),
            n => format!("{}*{n}", name(&presses[i])),
        });
        i += run;
    }
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use Button::*;

    #[test]
    fn reads_buttons_repeats_and_combos() {
        assert_eq!(
            parse("a  B,start up*3 a+b wait").unwrap(),
            vec![
                Press::of(vec![A]),
                Press::of(vec![B]),
                Press::of(vec![Start]),
                Press::of(vec![Up]),
                Press::of(vec![Up]),
                Press::of(vec![Up]),
                Press::of(vec![A, B]),
                Press::of(vec![]),
            ]
        );
        assert_eq!(
            parse("a:40 b+right:8*2 wait:60").unwrap(),
            vec![
                Press {
                    frames: Some(40),
                    ..Press::of(vec![A])
                },
                Press {
                    frames: Some(8),
                    ..Press::of(vec![B, Right])
                },
                Press {
                    frames: Some(8),
                    ..Press::of(vec![B, Right])
                },
                Press {
                    frames: Some(60),
                    ..Press::of(vec![])
                },
            ]
        );
        assert!(parse("r x2").unwrap_err().contains("`x2` isn't a button"));
        assert_eq!(parse("Rx2 l").unwrap().len(), 3);
        assert_eq!(parse("a+a").unwrap(), vec![Press::of(vec![A])]);
        assert_eq!(parse("wait*2").unwrap(), vec![Press::of(vec![]); 2]);
    }

    #[test]
    fn holds_each_button_its_own_frames() {
        let jump = parse("a:20+right:45").unwrap();
        assert_eq!(
            jump,
            vec![Press {
                frames: Some(45),
                shorter: vec![(A, 20)],
                ..Press::of(vec![A, Right])
            }]
        );
        assert_eq!(jump[0].frames_of(A, 16), 20);
        assert_eq!(jump[0].frames_of(Right, 16), 45);
        assert_eq!(describe(&jump), "a:20+right:45");
        // Written the other way round, or with the long one unnumbered.
        assert_eq!(describe(&parse("right:45+a:20").unwrap()), "right:45+a:20");
        assert_eq!(parse("a:30+right").unwrap(), parse("a+right:30").unwrap());
        assert_eq!(describe(&parse("a:30+right:30").unwrap()), "a+right:30");
        let plain = parse("a+right").unwrap();
        assert_eq!(plain[0].frames_of(A, 16), 16);
        assert!(
            parse("a:20+right:200")
                .unwrap_err()
                .contains("frames from 1")
        );
        assert!(
            parse("wait+a")
                .unwrap_err()
                .contains("`wait` isn't a button")
        );
    }

    #[test]
    fn explains_mistakes() {
        assert!(parse("").unwrap_err().contains("Say which"));
        assert!(parse("jump").unwrap_err().contains("`jump` isn't a button"));
        assert!(
            parse("aa")
                .unwrap_err()
                .contains("put spaces between presses, like `a a`")
        );
        assert!(parse("a*0").unwrap_err().contains("number from 1"));
        assert!(parse("a*lots").unwrap_err().contains("number from 1"));
        assert!(parse("a:0").unwrap_err().contains("frames from 1 to 120"));
        assert!(parse("a:121").unwrap_err().contains("frames from 1 to 120"));
        assert!(parse("a:").unwrap_err().contains("frames from 1 to 120"));
        assert!(parse("a*65").unwrap_err().contains("more than 64"));
        assert!(
            parse(&"a ".repeat(65))
                .unwrap_err()
                .contains("more than 64")
        );
        assert_eq!(parse(&"a ".repeat(64)).unwrap().len(), 64);
    }

    #[test]
    fn describes_presses_the_way_they_are_typed() {
        let presses = parse("a up up up a+b wait wait a b:40 b:40").unwrap();
        assert_eq!(describe(&presses), "a up*3 a+b wait*2 a b:40*2");
        assert_eq!(parse(&describe(&presses)).unwrap(), presses);
    }
}
