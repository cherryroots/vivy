//! What to press, typed as text: `a a up*3 a+b wait start`. Pure, no emulator.
//!
//! - a button name (`a`, `b`, `start`, `select`, `up`, `down`, `left`, `right`, or `u`,
//!   `d`, `l`, `r`) is one press
//! - `a+b` presses both at once
//! - `:40` after either holds it 40 frames, not the usual length (a charged shot, a higher
//!   jump)
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
}

impl Press {
    /// These buttons for the usual length.
    pub fn of(buttons: Vec<Button>) -> Press {
        Press {
            buttons,
            frames: None,
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
        let (what, frames) = match what.split_once(':') {
            Some((what, frames)) => {
                let frames: u32 = frames
                    .parse()
                    .ok()
                    .filter(|n| (1..=LONGEST).contains(n))
                    .ok_or_else(|| {
                        format!("`{word}`: put frames from 1 to {LONGEST} after `:`.")
                    })?;
                (what, Some(frames))
            }
            None => (what, None),
        };
        let press = if what.eq_ignore_ascii_case("wait") {
            Press {
                buttons: Vec::new(),
                frames,
            }
        } else {
            let mut buttons = Vec::new();
            for name in what.split('+') {
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
                }
            }
            Press { buttons, frames }
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
        let buttons = if press.buttons.is_empty() {
            "wait".to_string()
        } else {
            press
                .buttons
                .iter()
                .map(|b| b.name())
                .collect::<Vec<_>>()
                .join("+")
        };
        match press.frames {
            Some(frames) => format!("{buttons}:{frames}"),
            None => buttons,
        }
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
                    buttons: vec![A],
                    frames: Some(40)
                },
                Press {
                    buttons: vec![B, Right],
                    frames: Some(8)
                },
                Press {
                    buttons: vec![B, Right],
                    frames: Some(8)
                },
                Press {
                    buttons: vec![],
                    frames: Some(60)
                },
            ]
        );
        assert!(parse("r x2").unwrap_err().contains("`x2` isn't a button"));
        assert_eq!(parse("Rx2 l").unwrap().len(), 3);
        assert_eq!(parse("a+a").unwrap(), vec![Press::of(vec![A])]);
        assert_eq!(parse("wait*2").unwrap(), vec![Press::of(vec![]); 2]);
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
