//! The story flags worth knowing, roughly in the order the game reaches them. Each is a bit
//! in `wEventFlags`, numbered as in the disassembly's `constants/event_constants.asm`. The
//! game has about 500; these are the ones that say how far the story has got.

pub struct Flag {
    /// The disassembly's name, without `EVENT_`.
    pub name: &'static str,
    /// The bit's number from the start of `wEventFlags`.
    pub index: u16,
    /// What it means, for a prompt.
    pub said: &'static str,
}

const fn flag(name: &'static str, index: u16, said: &'static str) -> Flag {
    Flag { name, index, said }
}

pub const STORY: &[Flag] = &[
    flag("FOLLOWED_OAK_INTO_LAB", 0x000, "followed Oak into his lab"),
    flag("GOT_STARTER", 0x022, "chose a starter Pokémon"),
    flag(
        "BATTLED_RIVAL_IN_OAKS_LAB",
        0x023,
        "fought the rival in Oak's lab",
    ),
    flag(
        "GOT_OAKS_PARCEL",
        0x039,
        "picked up Oak's Parcel in Viridian City's mart",
    ),
    flag("OAK_GOT_PARCEL", 0x038, "gave Oak his parcel"),
    flag("GOT_POKEDEX", 0x025, "got the Pokédex"),
    flag("GOT_TOWN_MAP", 0x018, "got the Town Map"),
    flag("BEAT_BROCK", 0x077, "beat Brock (Pewter City gym)"),
    flag(
        "BEAT_CERULEAN_RIVAL",
        0x098,
        "beat the rival in Cerulean City",
    ),
    flag("GOT_SS_TICKET", 0x55c, "got the S.S. Ticket from Bill"),
    flag("BEAT_MISTY", 0x0bf, "beat Misty (Cerulean City gym)"),
    flag("GOT_HM01", 0x5e0, "got HM01 Cut on the S.S. Anne"),
    flag("SS_ANNE_LEFT", 0x5e2, "the S.S. Anne has sailed"),
    flag(
        "BEAT_LT_SURGE",
        0x167,
        "beat Lt. Surge (Vermilion City gym)",
    ),
    flag("GOT_BIKE_VOUCHER", 0x151, "got the Bike Voucher"),
    flag("GOT_BICYCLE", 0x0c0, "got the Bicycle"),
    flag("BEAT_ERIKA", 0x1a9, "beat Erika (Celadon City gym)"),
    flag(
        "ROCKET_DROPPED_LIFT_KEY",
        0x6a6,
        "found the Lift Key in the Rocket Hideout",
    ),
    flag(
        "BEAT_ROCKET_HIDEOUT_GIOVANNI",
        0x6a7,
        "beat Giovanni in the Rocket Hideout (Silph Scope)",
    ),
    flag(
        "RESCUED_MR_FUJI",
        0x4cf,
        "rescued Mr. Fuji from the Pokémon Tower",
    ),
    flag("GOT_POKE_FLUTE", 0x12a, "got the Poké Flute"),
    flag(
        "BEAT_ROUTE12_SNORLAX",
        0x48f,
        "moved the Snorlax on Route 12",
    ),
    flag(
        "BEAT_ROUTE16_SNORLAX",
        0x4c9,
        "moved the Snorlax on Route 16",
    ),
    flag("GOT_HM02", 0x4ce, "got HM02 Fly"),
    flag("BEAT_KOGA", 0x259, "beat Koga (Fuchsia City gym)"),
    flag(
        "GOT_HM04",
        0x238,
        "got HM04 Strength from the Safari Zone warden",
    ),
    flag("GOT_HM03", 0x880, "got HM03 Surf in the Safari Zone"),
    flag("GOT_HM05", 0x3d8, "got HM05 Flash"),
    flag(
        "BEAT_SILPH_CO_GIOVANNI",
        0x78f,
        "beat Giovanni in Silph Co.",
    ),
    flag("BEAT_SABRINA", 0x361, "beat Sabrina (Saffron City gym)"),
    flag("BEAT_BLAINE", 0x299, "beat Blaine (Cinnabar Island gym)"),
    flag(
        "BEAT_VIRIDIAN_GYM_GIOVANNI",
        0x051,
        "beat Giovanni (Viridian City gym)",
    ),
    flag("BEAT_CHAMPION_RIVAL", 0x901, "beat the rival, the Champion"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_are_distinct_and_inside_the_flag_bytes() {
        for (i, a) in STORY.iter().enumerate() {
            // wEventFlags is 320 bytes: 2560 bits.
            assert!(a.index < 2560, "{}", a.name);
            for b in &STORY[i + 1..] {
                assert_ne!(a.index, b.index, "{} and {}", a.name, b.name);
                assert_ne!(a.name, b.name);
            }
        }
    }
}
