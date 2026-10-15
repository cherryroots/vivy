//! The picture on each cannon message: the night sky over the arena, with the profile
//! pictures of everyone who fell, greyed out, and their names. The bot draws it itself
//! (with `util::svg`), so it costs nothing and is on in every game. It's attached as a
//! spoiler, like the names in the message.

use base64::Engine as _;
use serenity::all::{GuildId, UserId};
use tracing::debug;

use crate::core::BotCtx;
use crate::util::media;
use crate::util::shorten;
use crate::util::svg::{self, Style, Svg};

/// The picture's file name: Discord blurs a file whose name starts with `SPOILER_`.
pub const FILE: &str = "SPOILER_fallen.png";

const WIDTH: f32 = 600.0;
/// Each tribute's place in the grid.
const CELL: [f32; 2] = [136.0, 150.0];
const RADIUS: f32 = 42.0;
/// The size profile pictures are downloaded at.
const FACE_SIZE: u32 = 128;

const SKY_TOP: &str = "#0b1026";
const SKY_BOTTOM: &str = "#232a4d";
const STAR: &str = "#e8ecff";
const MOON: &str = "#f4efd8";
const RING: &str = "#6b7186";

/// Someone on the picture.
pub struct Fallen {
    pub name: String,
    pub kills: u32,
    /// Their profile picture as a `data:` URL, or `None` to draw their initial.
    pub face: Option<String>,
}

/// Draws the picture of `fallen` under `title`, like "The fallen of day 2". `None` when
/// nobody fell, or it couldn't be drawn.
pub async fn picture(
    bot: &BotCtx,
    guild: GuildId,
    title: &str,
    fallen: Vec<(u64, String, u32)>,
) -> Option<Vec<u8>> {
    if fallen.is_empty() {
        return None;
    }
    // The profile pictures download side by side.
    let downloads: Vec<_> = fallen
        .iter()
        .map(|&(id, _, _)| tokio::spawn(face(bot.clone(), guild, UserId::new(id))))
        .collect();
    let mut people = Vec::new();
    for ((_, name, kills), download) in fallen.into_iter().zip(downloads) {
        let face = match download.await {
            Ok(Ok(face)) => Some(face),
            Ok(Err(err)) => {
                debug!("couldn't load a fallen tribute's profile picture: {err:#}");
                None
            }
            Err(_) => None,
        };
        people.push(Fallen { name, kills, face });
    }
    let title = title.to_string();
    let drawn = tokio::task::spawn_blocking(move || svg::to_png(|| sky(&title, &people))).await;
    match drawn {
        Ok(Ok(png)) => Some(png),
        Ok(Err(err)) => {
            debug!("couldn't draw the fallen tributes: {err:#}");
            None
        }
        Err(_) => None,
    }
}

/// Someone's profile picture in this server, as a `data:` URL.
async fn face(bot: BotCtx, guild: GuildId, user: UserId) -> anyhow::Result<String> {
    let member = guild.member(&bot.http, user).await?;
    let url = member.avatar_url().unwrap_or_else(|| member.user.face());
    let png = media::download(&media::png_link(&url, FACE_SIZE), 4 * 1024 * 1024).await?;
    Ok(format!(
        "data:image/png;base64,{}",
        base64::prelude::BASE64_STANDARD.encode(png)
    ))
}

/// The picture as SVG.
pub fn sky(title: &str, fallen: &[Fallen]) -> String {
    let columns = match fallen.len() {
        n @ 0..=4 => n.max(1),
        5 | 6 => 3,
        _ => 4,
    };
    let rows = fallen.len().div_ceil(columns);
    let height = 104.0 + rows as f32 * CELL[1] + 8.0;

    let mut svg = Svg::new(WIDTH);
    svg.raw(format!(
        "<defs>\
         <linearGradient id=\"sky\" x1=\"0\" y1=\"0\" x2=\"0\" y2=\"1\">\
         <stop offset=\"0\" stop-color=\"{SKY_TOP}\"/><stop offset=\"1\" stop-color=\"{SKY_BOTTOM}\"/>\
         </linearGradient>\
         <filter id=\"grey\"><feColorMatrix type=\"saturate\" values=\"0\"/></filter>\
         </defs>\
         <rect width=\"{WIDTH}\" height=\"{height}\" rx=\"14\" fill=\"url(#sky)\"/>"
    ));
    stars(&mut svg, height, title.len() + fallen.len());
    // A crescent moon: a light circle with a sky-colored one over most of it.
    svg.raw(format!(
        "<circle cx=\"548\" cy=\"46\" r=\"20\" fill=\"{MOON}\"/>\
         <circle cx=\"558\" cy=\"40\" r=\"18\" fill=\"{SKY_TOP}\"/>"
    ));

    svg.text(
        WIDTH / 2.0,
        40.0,
        "CANNONS OVER THE ARENA",
        Style::new(11.0, svg::MUTED).weight(700).middle().spaced(),
    );
    svg.text(
        WIDTH / 2.0,
        72.0,
        title,
        Style::new(24.0, svg::TEXT).weight(700).middle(),
    );

    for (row, people) in fallen.chunks(columns).enumerate() {
        let left = WIDTH / 2.0 - people.len() as f32 * CELL[0] / 2.0;
        let top = 104.0 + row as f32 * CELL[1];
        for (column, person) in people.iter().enumerate() {
            let cx = left + (column as f32 + 0.5) * CELL[0];
            tribute(&mut svg, [cx, top], row * columns + column, person);
        }
    }
    svg.y = height;
    svg.finish()
}

/// One fallen tribute, centered on `cx` with the top at `top`.
fn tribute(svg: &mut Svg, [cx, top]: [f32; 2], index: usize, person: &Fallen) {
    let cy = top + RADIUS + 4.0;
    match &person.face {
        Some(face) => {
            let id = format!("face{index}");
            // The sky behind a faded picture would show through it.
            svg.raw(format!(
                "<circle cx=\"{cx}\" cy=\"{cy}\" r=\"{RADIUS}\" fill=\"{SKY_TOP}\"/>\
                 <clipPath id=\"{id}\"><circle cx=\"{cx}\" cy=\"{cy}\" r=\"{RADIUS}\"/></clipPath>\
                 <image href=\"{}\" x=\"{}\" y=\"{}\" width=\"{size}\" height=\"{size}\" \
                 preserveAspectRatio=\"xMidYMid slice\" clip-path=\"url(#{id})\" \
                 filter=\"url(#grey)\" opacity=\"0.75\"/>",
                svg::esc(face),
                cx - RADIUS,
                cy - RADIUS,
                size = RADIUS * 2.0,
            ));
        }
        None => {
            svg.raw(format!(
                "<circle cx=\"{cx}\" cy=\"{cy}\" r=\"{RADIUS}\" fill=\"{}\"/>",
                svg::ROW
            ));
            let initial: String = person.name.chars().take(1).collect();
            svg.text(
                cx,
                cy + 11.0,
                &initial.to_uppercase(),
                Style::new(30.0, svg::DIM).weight(700).middle(),
            );
        }
    }
    svg.raw(format!(
        "<circle cx=\"{cx}\" cy=\"{cy}\" r=\"{RADIUS}\" fill=\"none\" stroke=\"{RING}\" stroke-width=\"2\"/>"
    ));
    let name_y = cy + RADIUS + 24.0;
    svg.text(
        cx,
        name_y,
        &shorten(&person.name, 14),
        Style::new(14.0, svg::TEXT).weight(600).middle(),
    );
    if person.kills > 0 {
        let kills = match person.kills {
            1 => "1 kill".to_string(),
            n => format!("{n} kills"),
        };
        svg.text(
            cx,
            name_y + 18.0,
            &kills,
            Style::new(12.0, svg::MUTED).middle(),
        );
    }
}

/// Small stars scattered over the sky, the same for the same `seed`.
fn stars(svg: &mut Svg, height: f32, seed: usize) {
    let mut state = seed as u64 * 2_654_435_761 + 1;
    let mut next = || {
        // A small linear congruential generator: random enough for stars.
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as f32 / (1u64 << 31) as f32
    };
    for _ in 0..70 {
        let (x, y) = (next() * WIDTH, next() * height);
        let r = 0.5 + next() * 1.1;
        let opacity = 0.25 + next() * 0.6;
        svg.raw(format!(
            "<circle cx=\"{x:.1}\" cy=\"{y:.1}\" r=\"{r:.2}\" fill=\"{STAR}\" opacity=\"{opacity:.2}\"/>"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in profile picture: a light head and shoulders on a colored background.
    fn face(color: [u8; 3]) -> String {
        let image = image::RgbaImage::from_fn(64, 64, |x, y| {
            let (x, y) = (x as f32, y as f32);
            let head = (x - 32.0).powi(2) + (y - 26.0).powi(2) < 120.0;
            let shoulders = (x - 32.0).powi(2) / 2.0 + (y - 64.0).powi(2) < 260.0;
            let [r, g, b] = color;
            image::Rgba(if head || shoulders {
                [r / 3 + 170, g / 3 + 170, b / 3 + 170, 255]
            } else {
                [r, g, b, 255]
            })
        });
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        format!(
            "data:image/png;base64,{}",
            base64::prelude::BASE64_STANDARD.encode(png)
        )
    }

    fn fallen(name: &str, kills: u32, face: Option<String>) -> Fallen {
        Fallen {
            name: name.to_string(),
            kills,
            face,
        }
    }

    /// Also writes the pictures to `$FALLEN_MOCKUP_DIR` when it's set, to look at them.
    #[test]
    fn draws_the_fallen() {
        let day = vec![
            fallen("Cherry", 2, Some(face([64, 190, 200]))),
            fallen("Sam & <Jo>", 0, Some(face([220, 90, 120]))),
            fallen("Lou", 1, None),
        ];
        let svg = sky("The fallen of day 2", &day);
        assert!(svg.contains(">The fallen of day 2<"));
        assert!(svg.contains(">Cherry<") && svg.contains(">2 kills<"));
        assert!(svg.contains(">Sam &amp; &lt;Jo&gt;<"));
        assert!(svg.contains(">L<") && svg.contains(">1 kill<"));
        assert_eq!(svg.matches("filter=\"url(#grey)\"").count(), 2);

        let many: Vec<Fallen> = (0..7)
            .map(|i| {
                let face = (i % 3 != 2).then(|| face([40 * i as u8, 150, 220 - 25 * i as u8]));
                fallen(&format!("Tribute number {i}"), i, face)
            })
            .collect();
        let last = sky("The last of the fallen", &many);
        let one = sky("The fallen of day 1", &day[..1]);
        for (name, svg) in [("day", &svg), ("many", &last), ("one", &one)] {
            let png = svg::to_png(|| svg.clone()).unwrap();
            if let Ok(dir) = std::env::var("FALLEN_MOCKUP_DIR") {
                std::fs::write(format!("{dir}/fallen-{name}.png"), png).unwrap();
            }
        }
    }
}
