//! What to press, typed as text: `a a up*3 a+b wait start`. Pure, no emulator.
//!
//! - a button name (`a`, `b`, `start`, `select`, `up`, `down`, `left`, `right`, or `u`,
//!   `d`, `l`, `r`) is one press
//! - `a+b` presses both at once
//! - `*3` after either repeats it
//! - `wait` presses nothing for as long as a press takes
//!
//! Spaces or commas separate presses.

use super::emulator::Button;

/// The most presses in one go, so one command can't run for minutes.
pub const MOST: usize = 64;

/// One press: these buttons held together, then let go. No buttons is a wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Press(pub Vec<Button>);

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
        let press = if what.eq_ignore_ascii_case("wait") {
            Press(Vec::new())
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
            Press(buttons)
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
/// `a up*3 a+b`.
pub fn describe(presses: &[Press]) -> String {
    let name = |press: &Press| {
        if press.0.is_empty() {
            "wait".to_string()
        } else {
            press
                .0
                .iter()
                .map(|b| b.name())
                .collect::<Vec<_>>()
                .join("+")
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
                Press(vec![A]),
                Press(vec![B]),
                Press(vec![Start]),
                Press(vec![Up]),
                Press(vec![Up]),
                Press(vec![Up]),
                Press(vec![A, B]),
                Press(vec![]),
            ]
        );
        assert!(parse("r x2").unwrap_err().contains("`x2` isn't a button"));
        assert_eq!(parse("Rx2 l").unwrap().len(), 3);
        assert_eq!(parse("a+a").unwrap(), vec![Press(vec![A])]);
        assert_eq!(parse("wait*2").unwrap(), vec![Press(vec![]); 2]);
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
        let presses = parse("a up up up a+b wait wait a").unwrap();
        assert_eq!(describe(&presses), "a up*3 a+b wait*2 a");
        assert_eq!(parse(&describe(&presses)).unwrap(), presses);
    }
}
