//! The rules of one game, with no Discord or clock in it: the caller says how long each
//! pick took, so the tests can play a whole game in a few lines.
//!
//! Anyone in the channel can play: a player is whoever presses a button. Each round, the
//! first button a player presses is their answer. When the round ends, right answers score
//! 100 points plus up to 100 more for answering fast.

use std::collections::HashMap;

/// Points for a right answer, plus up to this much again for answering fast.
const POINTS: u32 = 100;

/// Someone who can be an answer: a quote's author, or one of the wrong choices.
#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    pub id: u64,
    pub name: String,
}

/// A message to guess the author of.
#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    pub author: u64,
    pub text: String,
    /// When it was posted, shown under the quote ("March 2025").
    pub posted: String,
    /// The channel it was posted in, shown under the quote when it isn't the channel the
    /// game is played in.
    pub place: Option<String>,
    /// A link to the message, shown with the answer.
    pub link: String,
}

/// One question: the quote, and the people to choose from (the author among them).
#[derive(Debug, Clone)]
pub struct Round {
    pub quote: Quote,
    pub choices: Vec<Person>,
    /// Player → (who they picked, how many milliseconds they took).
    picks: HashMap<u64, (u64, u64)>,
}

impl Round {
    pub fn new(quote: Quote, choices: Vec<Person>) -> Self {
        Round {
            quote,
            choices,
            picks: HashMap::new(),
        }
    }

    /// The person who wrote the quote.
    pub fn author(&self) -> &Person {
        self.choices
            .iter()
            .find(|c| c.id == self.quote.author)
            .expect("a round's choices include its author")
    }
}

/// What happened to a button press.
#[derive(Debug, PartialEq)]
pub enum Pick {
    /// Their answer is in.
    Locked,
    /// They had already answered this round with this person.
    Already(u64),
    /// The round is over (an old message's button).
    TooLate,
}

/// How a round went, for the answer message.
#[derive(Debug, PartialEq)]
pub struct Outcome {
    /// Who got it right and their points, fastest first.
    pub right: Vec<(String, u32)>,
    /// Who got it wrong.
    pub wrong: Vec<String>,
}

pub struct Game {
    rounds: Vec<Round>,
    /// The round being answered, or `rounds.len()` once the game is over.
    current: usize,
    /// Whether the current round still takes answers.
    open: bool,
    answer_ms: u64,
    /// Every player's name and score, in the order they first answered.
    players: Vec<(u64, String, u32)>,
}

impl Game {
    pub fn new(rounds: Vec<Round>, answer_seconds: u64) -> Self {
        Game {
            rounds,
            current: 0,
            open: true,
            answer_ms: answer_seconds.max(5) * 1000,
            players: Vec::new(),
        }
    }

    /// The number of the current round (from 0), and the round.
    pub fn round(&self) -> Option<(usize, &Round)> {
        self.rounds.get(self.current).map(|r| (self.current, r))
    }

    pub fn total(&self) -> usize {
        self.rounds.len()
    }

    /// Records a button press on round `index`: `player` (with their name) picked `choice`
    /// after `took` milliseconds.
    pub fn pick(&mut self, index: usize, player: u64, name: &str, choice: u64, took: u64) -> Pick {
        if index != self.current || !self.open {
            return Pick::TooLate;
        }
        let Some(round) = self.rounds.get_mut(index) else {
            return Pick::TooLate;
        };
        if let Some(&(earlier, _)) = round.picks.get(&player) {
            return Pick::Already(earlier);
        }
        if !round.choices.iter().any(|c| c.id == choice) {
            return Pick::TooLate;
        }
        round.picks.insert(player, (choice, took));
        match self.players.iter_mut().find(|p| p.0 == player) {
            Some(p) => p.1 = name.to_string(),
            None => self.players.push((player, name.to_string(), 0)),
        }
        Pick::Locked
    }

    /// How many players have answered the current round.
    pub fn answers(&self) -> usize {
        self.round().map_or(0, |(_, r)| r.picks.len())
    }

    /// Ends the current round: scores it and stops taking answers.
    pub fn close_round(&mut self) -> Outcome {
        self.open = false;
        let Some(round) = self.rounds.get(self.current) else {
            return Outcome {
                right: Vec::new(),
                wrong: Vec::new(),
            };
        };
        let mut picks: Vec<(u64, u64, u64)> = round
            .picks
            .iter()
            .map(|(&player, &(choice, took))| (player, choice, took))
            .collect();
        picks.sort_by_key(|&(_, _, took)| took);

        let mut right = Vec::new();
        let mut wrong = Vec::new();
        for (player, choice, took) in picks {
            let Some(entry) = self.players.iter_mut().find(|p| p.0 == player) else {
                continue;
            };
            if choice == round.quote.author {
                let gained = points(took, self.answer_ms);
                entry.2 += gained;
                right.push((entry.1.clone(), gained));
            } else {
                wrong.push(entry.1.clone());
            }
        }
        Outcome { right, wrong }
    }

    /// Moves on to the next round. Returns false when the game is over.
    pub fn next_round(&mut self) -> bool {
        self.current += 1;
        self.open = true;
        self.current < self.rounds.len()
    }

    /// Everyone who played, best score first.
    pub fn standings(&self) -> Vec<(String, u32)> {
        let mut players: Vec<(String, u32)> = self
            .players
            .iter()
            .map(|(_, name, score)| (name.clone(), *score))
            .collect();
        players.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
        players
    }
}

/// Points for a right answer that took `took` of `answer_ms`: 100, plus up to 100 more for
/// answering fast.
fn points(took: u64, answer_ms: u64) -> u32 {
    let left = answer_ms.saturating_sub(took);
    POINTS + (u64::from(POINTS) * left / answer_ms.max(1)) as u32
}

/// Picks up to `count` rounds from `quotes`, with up to 4 choices each: the author and
/// others from `people`. `shuffle` puts a list in random order (the tests pass one that
/// doesn't, so the result is known).
///
/// Nobody is the answer more than twice while someone else still could be, so one chatty
/// person doesn't make every answer the same.
pub fn make_rounds(
    quotes: Vec<Quote>,
    people: &[Person],
    count: usize,
    mut shuffle: impl FnMut(&mut Vec<Person>),
) -> Vec<Round> {
    let mut used: HashMap<u64, usize> = HashMap::new();
    let mut picked = Vec::new();
    let mut later = Vec::new();
    for quote in quotes {
        let times = used.entry(quote.author).or_default();
        if *times >= 2 {
            later.push(quote);
            continue;
        }
        *times += 1;
        picked.push(quote);
    }
    // Not enough variety: fill up with the rest.
    picked.extend(later);
    picked.truncate(count);

    picked
        .into_iter()
        .filter_map(|quote| {
            let author = people.iter().find(|p| p.id == quote.author)?.clone();
            let mut others: Vec<Person> = people
                .iter()
                .filter(|p| p.id != author.id)
                .cloned()
                .collect();
            shuffle(&mut others);
            others.truncate(3);
            if others.is_empty() {
                return None;
            }
            let mut choices = others;
            choices.push(author);
            shuffle(&mut choices);
            Some(Round::new(quote, choices))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(id: u64) -> Person {
        Person {
            id,
            name: format!("person {id}"),
        }
    }

    fn quote(author: u64) -> Quote {
        Quote {
            author,
            text: format!("something {author} said"),
            posted: "March 2025".into(),
            place: None,
            link: "https://discord.com/channels/1/2/3".into(),
        }
    }

    fn no_shuffle(_: &mut Vec<Person>) {}

    #[test]
    fn rounds_have_the_author_and_up_to_three_others() {
        let people: Vec<Person> = (1..=6).map(person).collect();
        let rounds = make_rounds(vec![quote(1), quote(2)], &people, 10, no_shuffle);
        assert_eq!(rounds.len(), 2);
        for round in &rounds {
            assert_eq!(round.choices.len(), 4);
            assert_eq!(round.author().id, round.quote.author);
        }
    }

    #[test]
    fn nobody_is_the_answer_more_than_twice_while_others_could_be() {
        let people: Vec<Person> = (1..=3).map(person).collect();
        let quotes = vec![quote(1), quote(1), quote(1), quote(1), quote(2), quote(3)];
        let rounds = make_rounds(quotes, &people, 4, no_shuffle);
        let authors: Vec<u64> = rounds.iter().map(|r| r.quote.author).collect();
        assert_eq!(authors, vec![1, 1, 2, 3]);
    }

    #[test]
    fn a_quote_needs_someone_else_to_choose() {
        let rounds = make_rounds(vec![quote(1)], &[person(1)], 10, no_shuffle);
        assert!(rounds.is_empty());
    }

    #[test]
    fn fast_right_answers_score_more() {
        assert_eq!(points(0, 20_000), 200);
        assert_eq!(points(10_000, 20_000), 150);
        assert_eq!(points(20_000, 20_000), 100);
        assert_eq!(points(30_000, 20_000), 100);
    }

    #[test]
    fn a_whole_game() {
        let people: Vec<Person> = (1..=4).map(person).collect();
        let rounds = make_rounds(vec![quote(1), quote(2)], &people, 2, no_shuffle);
        let mut game = Game::new(rounds, 20);

        // Round 1: Ann is right at once, Bo is wrong, and Ann's second press doesn't count.
        assert_eq!(game.pick(0, 10, "Ann", 1, 0), Pick::Locked);
        assert_eq!(game.pick(0, 10, "Ann", 2, 100), Pick::Already(1));
        assert_eq!(game.pick(0, 11, "Bo", 3, 5_000), Pick::Locked);
        assert_eq!(game.answers(), 2);
        let outcome = game.close_round();
        assert_eq!(outcome.right, vec![("Ann".to_string(), 200)]);
        assert_eq!(outcome.wrong, vec!["Bo".to_string()]);
        assert_eq!(game.pick(0, 12, "Cy", 1, 0), Pick::TooLate);

        // Round 2: Bo is right halfway through; a press on round 1's message is too late.
        assert!(game.next_round());
        assert_eq!(game.pick(0, 11, "Bo", 1, 0), Pick::TooLate);
        assert_eq!(game.pick(1, 11, "Bo", 2, 10_000), Pick::Locked);
        game.close_round();
        assert!(!game.next_round());
        assert!(game.round().is_none());

        assert_eq!(
            game.standings(),
            vec![("Ann".to_string(), 200), ("Bo".to_string(), 150)]
        );
    }

    #[test]
    fn a_pick_must_be_one_of_the_choices() {
        let people: Vec<Person> = (1..=4).map(person).collect();
        let rounds = make_rounds(vec![quote(1)], &people, 1, no_shuffle);
        let mut game = Game::new(rounds, 20);
        assert_eq!(game.pick(0, 10, "Ann", 99, 0), Pick::TooLate);
        assert_eq!(game.answers(), 0);
    }
}
