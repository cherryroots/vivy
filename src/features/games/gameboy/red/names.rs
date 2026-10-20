//! Pokémon Red's names for its numbers: species, items and maps (with their sizes). Made
//! from the constants of the pret/pokered disassembly (github.com/pret/pokered), which lists
//! them in the game's own order. `None` is a number the game doesn't use.

/// Species by the game's own number (not the Pokédex's).
pub const SPECIES: [Option<&str>; 256] = [
    None,                      // 0x00
    Some("Rhydon"),            // 0x01
    Some("Kangaskhan"),        // 0x02
    Some("Nidoran♂"),          // 0x03
    Some("Clefairy"),          // 0x04
    Some("Spearow"),           // 0x05
    Some("Voltorb"),           // 0x06
    Some("Nidoking"),          // 0x07
    Some("Slowbro"),           // 0x08
    Some("Ivysaur"),           // 0x09
    Some("Exeggutor"),         // 0x0a
    Some("Lickitung"),         // 0x0b
    Some("Exeggcute"),         // 0x0c
    Some("Grimer"),            // 0x0d
    Some("Gengar"),            // 0x0e
    Some("Nidoran♀"),          // 0x0f
    Some("Nidoqueen"),         // 0x10
    Some("Cubone"),            // 0x11
    Some("Rhyhorn"),           // 0x12
    Some("Lapras"),            // 0x13
    Some("Arcanine"),          // 0x14
    Some("Mew"),               // 0x15
    Some("Gyarados"),          // 0x16
    Some("Shellder"),          // 0x17
    Some("Tentacool"),         // 0x18
    Some("Gastly"),            // 0x19
    Some("Scyther"),           // 0x1a
    Some("Staryu"),            // 0x1b
    Some("Blastoise"),         // 0x1c
    Some("Pinsir"),            // 0x1d
    Some("Tangela"),           // 0x1e
    None,                      // 0x1f
    None,                      // 0x20
    Some("Growlithe"),         // 0x21
    Some("Onix"),              // 0x22
    Some("Fearow"),            // 0x23
    Some("Pidgey"),            // 0x24
    Some("Slowpoke"),          // 0x25
    Some("Kadabra"),           // 0x26
    Some("Graveler"),          // 0x27
    Some("Chansey"),           // 0x28
    Some("Machoke"),           // 0x29
    Some("Mr. Mime"),          // 0x2a
    Some("Hitmonlee"),         // 0x2b
    Some("Hitmonchan"),        // 0x2c
    Some("Arbok"),             // 0x2d
    Some("Parasect"),          // 0x2e
    Some("Psyduck"),           // 0x2f
    Some("Drowzee"),           // 0x30
    Some("Golem"),             // 0x31
    None,                      // 0x32
    Some("Magmar"),            // 0x33
    None,                      // 0x34
    Some("Electabuzz"),        // 0x35
    Some("Magneton"),          // 0x36
    Some("Koffing"),           // 0x37
    None,                      // 0x38
    Some("Mankey"),            // 0x39
    Some("Seel"),              // 0x3a
    Some("Diglett"),           // 0x3b
    Some("Tauros"),            // 0x3c
    None,                      // 0x3d
    None,                      // 0x3e
    None,                      // 0x3f
    Some("Farfetchd"),         // 0x40
    Some("Venonat"),           // 0x41
    Some("Dragonite"),         // 0x42
    None,                      // 0x43
    None,                      // 0x44
    None,                      // 0x45
    Some("Doduo"),             // 0x46
    Some("Poliwag"),           // 0x47
    Some("Jynx"),              // 0x48
    Some("Moltres"),           // 0x49
    Some("Articuno"),          // 0x4a
    Some("Zapdos"),            // 0x4b
    Some("Ditto"),             // 0x4c
    Some("Meowth"),            // 0x4d
    Some("Krabby"),            // 0x4e
    None,                      // 0x4f
    None,                      // 0x50
    None,                      // 0x51
    Some("Vulpix"),            // 0x52
    Some("Ninetales"),         // 0x53
    Some("Pikachu"),           // 0x54
    Some("Raichu"),            // 0x55
    None,                      // 0x56
    None,                      // 0x57
    Some("Dratini"),           // 0x58
    Some("Dragonair"),         // 0x59
    Some("Kabuto"),            // 0x5a
    Some("Kabutops"),          // 0x5b
    Some("Horsea"),            // 0x5c
    Some("Seadra"),            // 0x5d
    None,                      // 0x5e
    None,                      // 0x5f
    Some("Sandshrew"),         // 0x60
    Some("Sandslash"),         // 0x61
    Some("Omanyte"),           // 0x62
    Some("Omastar"),           // 0x63
    Some("Jigglypuff"),        // 0x64
    Some("Wigglytuff"),        // 0x65
    Some("Eevee"),             // 0x66
    Some("Flareon"),           // 0x67
    Some("Jolteon"),           // 0x68
    Some("Vaporeon"),          // 0x69
    Some("Machop"),            // 0x6a
    Some("Zubat"),             // 0x6b
    Some("Ekans"),             // 0x6c
    Some("Paras"),             // 0x6d
    Some("Poliwhirl"),         // 0x6e
    Some("Poliwrath"),         // 0x6f
    Some("Weedle"),            // 0x70
    Some("Kakuna"),            // 0x71
    Some("Beedrill"),          // 0x72
    None,                      // 0x73
    Some("Dodrio"),            // 0x74
    Some("Primeape"),          // 0x75
    Some("Dugtrio"),           // 0x76
    Some("Venomoth"),          // 0x77
    Some("Dewgong"),           // 0x78
    None,                      // 0x79
    None,                      // 0x7a
    Some("Caterpie"),          // 0x7b
    Some("Metapod"),           // 0x7c
    Some("Butterfree"),        // 0x7d
    Some("Machamp"),           // 0x7e
    None,                      // 0x7f
    Some("Golduck"),           // 0x80
    Some("Hypno"),             // 0x81
    Some("Golbat"),            // 0x82
    Some("Mewtwo"),            // 0x83
    Some("Snorlax"),           // 0x84
    Some("Magikarp"),          // 0x85
    None,                      // 0x86
    None,                      // 0x87
    Some("Muk"),               // 0x88
    None,                      // 0x89
    Some("Kingler"),           // 0x8a
    Some("Cloyster"),          // 0x8b
    None,                      // 0x8c
    Some("Electrode"),         // 0x8d
    Some("Clefable"),          // 0x8e
    Some("Weezing"),           // 0x8f
    Some("Persian"),           // 0x90
    Some("Marowak"),           // 0x91
    None,                      // 0x92
    Some("Haunter"),           // 0x93
    Some("Abra"),              // 0x94
    Some("Alakazam"),          // 0x95
    Some("Pidgeotto"),         // 0x96
    Some("Pidgeot"),           // 0x97
    Some("Starmie"),           // 0x98
    Some("Bulbasaur"),         // 0x99
    Some("Venusaur"),          // 0x9a
    Some("Tentacruel"),        // 0x9b
    None,                      // 0x9c
    Some("Goldeen"),           // 0x9d
    Some("Seaking"),           // 0x9e
    None,                      // 0x9f
    None,                      // 0xa0
    None,                      // 0xa1
    None,                      // 0xa2
    Some("Ponyta"),            // 0xa3
    Some("Rapidash"),          // 0xa4
    Some("Rattata"),           // 0xa5
    Some("Raticate"),          // 0xa6
    Some("Nidorino"),          // 0xa7
    Some("Nidorina"),          // 0xa8
    Some("Geodude"),           // 0xa9
    Some("Porygon"),           // 0xaa
    Some("Aerodactyl"),        // 0xab
    None,                      // 0xac
    Some("Magnemite"),         // 0xad
    None,                      // 0xae
    None,                      // 0xaf
    Some("Charmander"),        // 0xb0
    Some("Squirtle"),          // 0xb1
    Some("Charmeleon"),        // 0xb2
    Some("Wartortle"),         // 0xb3
    Some("Charizard"),         // 0xb4
    None,                      // 0xb5
    Some("Fossil Kabutops"),   // 0xb6
    Some("Fossil Aerodactyl"), // 0xb7
    Some("Mon Ghost"),         // 0xb8
    Some("Oddish"),            // 0xb9
    Some("Gloom"),             // 0xba
    Some("Vileplume"),         // 0xbb
    Some("Bellsprout"),        // 0xbc
    Some("Weepinbell"),        // 0xbd
    Some("Victreebel"),        // 0xbe
    None,                      // 0xbf
    None,                      // 0xc0
    None,                      // 0xc1
    None,                      // 0xc2
    None,                      // 0xc3
    None,                      // 0xc4
    None,                      // 0xc5
    None,                      // 0xc6
    None,                      // 0xc7
    None,                      // 0xc8
    None,                      // 0xc9
    None,                      // 0xca
    None,                      // 0xcb
    None,                      // 0xcc
    None,                      // 0xcd
    None,                      // 0xce
    None,                      // 0xcf
    None,                      // 0xd0
    None,                      // 0xd1
    None,                      // 0xd2
    None,                      // 0xd3
    None,                      // 0xd4
    None,                      // 0xd5
    None,                      // 0xd6
    None,                      // 0xd7
    None,                      // 0xd8
    None,                      // 0xd9
    None,                      // 0xda
    None,                      // 0xdb
    None,                      // 0xdc
    None,                      // 0xdd
    None,                      // 0xde
    None,                      // 0xdf
    None,                      // 0xe0
    None,                      // 0xe1
    None,                      // 0xe2
    None,                      // 0xe3
    None,                      // 0xe4
    None,                      // 0xe5
    None,                      // 0xe6
    None,                      // 0xe7
    None,                      // 0xe8
    None,                      // 0xe9
    None,                      // 0xea
    None,                      // 0xeb
    None,                      // 0xec
    None,                      // 0xed
    None,                      // 0xee
    None,                      // 0xef
    None,                      // 0xf0
    None,                      // 0xf1
    None,                      // 0xf2
    None,                      // 0xf3
    None,                      // 0xf4
    None,                      // 0xf5
    None,                      // 0xf6
    None,                      // 0xf7
    None,                      // 0xf8
    None,                      // 0xf9
    None,                      // 0xfa
    None,                      // 0xfb
    None,                      // 0xfc
    None,                      // 0xfd
    None,                      // 0xfe
    None,                      // 0xff
];

/// Items, then HMs from 0xC4 and TMs from 0xC9.
pub const ITEMS: [Option<&str>; 256] = [
    None,                      // 0x00
    Some("Master Ball"),       // 0x01
    Some("Ultra Ball"),        // 0x02
    Some("Great Ball"),        // 0x03
    Some("Poké Ball"),         // 0x04
    Some("Town Map"),          // 0x05
    Some("Bicycle"),           // 0x06
    Some("Surfboard"),         // 0x07
    Some("Safari Ball"),       // 0x08
    Some("Pokédex"),           // 0x09
    Some("Moon Stone"),        // 0x0a
    Some("Antidote"),          // 0x0b
    Some("Burn Heal"),         // 0x0c
    Some("Ice Heal"),          // 0x0d
    Some("Awakening"),         // 0x0e
    Some("Parlyz Heal"),       // 0x0f
    Some("Full Restore"),      // 0x10
    Some("Max Potion"),        // 0x11
    Some("Hyper Potion"),      // 0x12
    Some("Super Potion"),      // 0x13
    Some("Potion"),            // 0x14
    Some("Boulderbadge"),      // 0x15
    Some("Cascadebadge"),      // 0x16
    Some("Thunderbadge"),      // 0x17
    Some("Rainbowbadge"),      // 0x18
    Some("Soulbadge"),         // 0x19
    Some("Marshbadge"),        // 0x1a
    Some("Volcanobadge"),      // 0x1b
    Some("Earthbadge"),        // 0x1c
    Some("Escape Rope"),       // 0x1d
    Some("Repel"),             // 0x1e
    Some("Old Amber"),         // 0x1f
    Some("Fire Stone"),        // 0x20
    Some("Thunder Stone"),     // 0x21
    Some("Water Stone"),       // 0x22
    Some("HP Up"),             // 0x23
    Some("Protein"),           // 0x24
    Some("Iron"),              // 0x25
    Some("Carbos"),            // 0x26
    Some("Calcium"),           // 0x27
    Some("Rare Candy"),        // 0x28
    Some("Dome Fossil"),       // 0x29
    Some("Helix Fossil"),      // 0x2a
    Some("Secret Key"),        // 0x2b
    Some("Item 2c"),           // 0x2c
    Some("Bike Voucher"),      // 0x2d
    Some("X Accuracy"),        // 0x2e
    Some("Leaf Stone"),        // 0x2f
    Some("Card Key"),          // 0x30
    Some("Nugget"),            // 0x31
    Some("Item 32"),           // 0x32
    Some("Poké Doll"),         // 0x33
    Some("Full Heal"),         // 0x34
    Some("Revive"),            // 0x35
    Some("Max Revive"),        // 0x36
    Some("Guard Spec"),        // 0x37
    Some("Super Repel"),       // 0x38
    Some("Max Repel"),         // 0x39
    Some("Dire Hit"),          // 0x3a
    Some("Coin"),              // 0x3b
    Some("Fresh Water"),       // 0x3c
    Some("Soda Pop"),          // 0x3d
    Some("Lemonade"),          // 0x3e
    Some("S S Ticket"),        // 0x3f
    Some("Gold Teeth"),        // 0x40
    Some("X Attack"),          // 0x41
    Some("X Defend"),          // 0x42
    Some("X Speed"),           // 0x43
    Some("X Special"),         // 0x44
    Some("Coin Case"),         // 0x45
    Some("Oak's Parcel"),      // 0x46
    Some("Itemfinder"),        // 0x47
    Some("Silph Scope"),       // 0x48
    Some("Poké Flute"),        // 0x49
    Some("Lift Key"),          // 0x4a
    Some("Exp All"),           // 0x4b
    Some("Old Rod"),           // 0x4c
    Some("Good Rod"),          // 0x4d
    Some("Super Rod"),         // 0x4e
    Some("PP Up"),             // 0x4f
    Some("Ether"),             // 0x50
    Some("Max Ether"),         // 0x51
    Some("Elixer"),            // 0x52
    Some("Max Elixer"),        // 0x53
    None,                      // 0x54
    None,                      // 0x55
    None,                      // 0x56
    None,                      // 0x57
    None,                      // 0x58
    None,                      // 0x59
    None,                      // 0x5a
    None,                      // 0x5b
    None,                      // 0x5c
    None,                      // 0x5d
    None,                      // 0x5e
    None,                      // 0x5f
    None,                      // 0x60
    None,                      // 0x61
    None,                      // 0x62
    None,                      // 0x63
    None,                      // 0x64
    None,                      // 0x65
    None,                      // 0x66
    None,                      // 0x67
    None,                      // 0x68
    None,                      // 0x69
    None,                      // 0x6a
    None,                      // 0x6b
    None,                      // 0x6c
    None,                      // 0x6d
    None,                      // 0x6e
    None,                      // 0x6f
    None,                      // 0x70
    None,                      // 0x71
    None,                      // 0x72
    None,                      // 0x73
    None,                      // 0x74
    None,                      // 0x75
    None,                      // 0x76
    None,                      // 0x77
    None,                      // 0x78
    None,                      // 0x79
    None,                      // 0x7a
    None,                      // 0x7b
    None,                      // 0x7c
    None,                      // 0x7d
    None,                      // 0x7e
    None,                      // 0x7f
    None,                      // 0x80
    None,                      // 0x81
    None,                      // 0x82
    None,                      // 0x83
    None,                      // 0x84
    None,                      // 0x85
    None,                      // 0x86
    None,                      // 0x87
    None,                      // 0x88
    None,                      // 0x89
    None,                      // 0x8a
    None,                      // 0x8b
    None,                      // 0x8c
    None,                      // 0x8d
    None,                      // 0x8e
    None,                      // 0x8f
    None,                      // 0x90
    None,                      // 0x91
    None,                      // 0x92
    None,                      // 0x93
    None,                      // 0x94
    None,                      // 0x95
    None,                      // 0x96
    None,                      // 0x97
    None,                      // 0x98
    None,                      // 0x99
    None,                      // 0x9a
    None,                      // 0x9b
    None,                      // 0x9c
    None,                      // 0x9d
    None,                      // 0x9e
    None,                      // 0x9f
    None,                      // 0xa0
    None,                      // 0xa1
    None,                      // 0xa2
    None,                      // 0xa3
    None,                      // 0xa4
    None,                      // 0xa5
    None,                      // 0xa6
    None,                      // 0xa7
    None,                      // 0xa8
    None,                      // 0xa9
    None,                      // 0xaa
    None,                      // 0xab
    None,                      // 0xac
    None,                      // 0xad
    None,                      // 0xae
    None,                      // 0xaf
    None,                      // 0xb0
    None,                      // 0xb1
    None,                      // 0xb2
    None,                      // 0xb3
    None,                      // 0xb4
    None,                      // 0xb5
    None,                      // 0xb6
    None,                      // 0xb7
    None,                      // 0xb8
    None,                      // 0xb9
    None,                      // 0xba
    None,                      // 0xbb
    None,                      // 0xbc
    None,                      // 0xbd
    None,                      // 0xbe
    None,                      // 0xbf
    None,                      // 0xc0
    None,                      // 0xc1
    None,                      // 0xc2
    None,                      // 0xc3
    Some("HM01 Cut"),          // 0xc4
    Some("HM02 Fly"),          // 0xc5
    Some("HM03 Surf"),         // 0xc6
    Some("HM04 Strength"),     // 0xc7
    Some("HM05 Flash"),        // 0xc8
    Some("TM01 Mega Punch"),   // 0xc9
    Some("TM02 Razor Wind"),   // 0xca
    Some("TM03 Swords Dance"), // 0xcb
    Some("TM04 Whirlwind"),    // 0xcc
    Some("TM05 Mega Kick"),    // 0xcd
    Some("TM06 Toxic"),        // 0xce
    Some("TM07 Horn Drill"),   // 0xcf
    Some("TM08 Body Slam"),    // 0xd0
    Some("TM09 Take Down"),    // 0xd1
    Some("TM10 Double Edge"),  // 0xd2
    Some("TM11 Bubblebeam"),   // 0xd3
    Some("TM12 Water Gun"),    // 0xd4
    Some("TM13 Ice Beam"),     // 0xd5
    Some("TM14 Blizzard"),     // 0xd6
    Some("TM15 Hyper Beam"),   // 0xd7
    Some("TM16 Pay Day"),      // 0xd8
    Some("TM17 Submission"),   // 0xd9
    Some("TM18 Counter"),      // 0xda
    Some("TM19 Seismic Toss"), // 0xdb
    Some("TM20 Rage"),         // 0xdc
    Some("TM21 Mega Drain"),   // 0xdd
    Some("TM22 Solarbeam"),    // 0xde
    Some("TM23 Dragon Rage"),  // 0xdf
    Some("TM24 Thunderbolt"),  // 0xe0
    Some("TM25 Thunder"),      // 0xe1
    Some("TM26 Earthquake"),   // 0xe2
    Some("TM27 Fissure"),      // 0xe3
    Some("TM28 Dig"),          // 0xe4
    Some("TM29 Psychic M"),    // 0xe5
    Some("TM30 Teleport"),     // 0xe6
    Some("TM31 Mimic"),        // 0xe7
    Some("TM32 Double Team"),  // 0xe8
    Some("TM33 Reflect"),      // 0xe9
    Some("TM34 Bide"),         // 0xea
    Some("TM35 Metronome"),    // 0xeb
    Some("TM36 Selfdestruct"), // 0xec
    Some("TM37 Egg Bomb"),     // 0xed
    Some("TM38 Fire Blast"),   // 0xee
    Some("TM39 Swift"),        // 0xef
    Some("TM40 Skull Bash"),   // 0xf0
    Some("TM41 Softboiled"),   // 0xf1
    Some("TM42 Dream Eater"),  // 0xf2
    Some("TM43 Sky Attack"),   // 0xf3
    Some("TM44 Rest"),         // 0xf4
    Some("TM45 Thunder Wave"), // 0xf5
    Some("TM46 Psywave"),      // 0xf6
    Some("TM47 Explosion"),    // 0xf7
    Some("TM48 Rock Slide"),   // 0xf8
    Some("TM49 Tri Attack"),   // 0xf9
    Some("TM50 Substitute"),   // 0xfa
    None,                      // 0xfb
    None,                      // 0xfc
    None,                      // 0xfd
    None,                      // 0xfe
    None,                      // 0xff
];

/// Each map's name and its size in blocks of 2×2 steps (width, height).
pub const MAPS: [Option<(&str, u8, u8)>; 256] = [
    Some(("Pallet Town", 10, 9)),                  // 0x00
    Some(("Viridian City", 20, 18)),               // 0x01
    Some(("Pewter City", 20, 18)),                 // 0x02
    Some(("Cerulean City", 20, 18)),               // 0x03
    Some(("Lavender Town", 10, 9)),                // 0x04
    Some(("Vermilion City", 20, 18)),              // 0x05
    Some(("Celadon City", 25, 18)),                // 0x06
    Some(("Fuchsia City", 20, 18)),                // 0x07
    Some(("Cinnabar Island", 10, 9)),              // 0x08
    Some(("Indigo Plateau", 10, 9)),               // 0x09
    Some(("Saffron City", 20, 18)),                // 0x0a
    None,                                          // 0x0b
    Some(("Route 1", 10, 18)),                     // 0x0c
    Some(("Route 2", 10, 36)),                     // 0x0d
    Some(("Route 3", 35, 9)),                      // 0x0e
    Some(("Route 4", 45, 9)),                      // 0x0f
    Some(("Route 5", 10, 18)),                     // 0x10
    Some(("Route 6", 10, 18)),                     // 0x11
    Some(("Route 7", 10, 9)),                      // 0x12
    Some(("Route 8", 30, 9)),                      // 0x13
    Some(("Route 9", 30, 9)),                      // 0x14
    Some(("Route 10", 10, 36)),                    // 0x15
    Some(("Route 11", 30, 9)),                     // 0x16
    Some(("Route 12", 10, 54)),                    // 0x17
    Some(("Route 13", 30, 9)),                     // 0x18
    Some(("Route 14", 10, 27)),                    // 0x19
    Some(("Route 15", 30, 9)),                     // 0x1a
    Some(("Route 16", 20, 9)),                     // 0x1b
    Some(("Route 17", 10, 72)),                    // 0x1c
    Some(("Route 18", 25, 9)),                     // 0x1d
    Some(("Route 19", 10, 27)),                    // 0x1e
    Some(("Route 20", 50, 9)),                     // 0x1f
    Some(("Route 21", 10, 45)),                    // 0x20
    Some(("Route 22", 20, 9)),                     // 0x21
    Some(("Route 23", 10, 72)),                    // 0x22
    Some(("Route 24", 10, 18)),                    // 0x23
    Some(("Route 25", 30, 9)),                     // 0x24
    Some(("Red's House 1F", 4, 4)),                // 0x25
    Some(("Red's House 2F", 4, 4)),                // 0x26
    Some(("Blue's House", 4, 4)),                  // 0x27
    Some(("Oak's Lab", 5, 6)),                     // 0x28
    Some(("Viridian Pokémon Center", 7, 4)),       // 0x29
    Some(("Viridian Mart", 4, 4)),                 // 0x2a
    Some(("Viridian School House", 4, 4)),         // 0x2b
    Some(("Viridian Nickname House", 4, 4)),       // 0x2c
    Some(("Viridian Gym", 10, 9)),                 // 0x2d
    Some(("Digletts Cave Route 2", 4, 4)),         // 0x2e
    Some(("Viridian Forest North Gate", 5, 4)),    // 0x2f
    Some(("Route 2 Trade House", 4, 4)),           // 0x30
    Some(("Route 2 Gate", 5, 4)),                  // 0x31
    Some(("Viridian Forest South Gate", 5, 4)),    // 0x32
    Some(("Viridian Forest", 17, 24)),             // 0x33
    Some(("Museum 1F", 10, 4)),                    // 0x34
    Some(("Museum 2F", 7, 4)),                     // 0x35
    Some(("Pewter Gym", 5, 7)),                    // 0x36
    Some(("Pewter Nidoran House", 4, 4)),          // 0x37
    Some(("Pewter Mart", 4, 4)),                   // 0x38
    Some(("Pewter Speech House", 4, 4)),           // 0x39
    Some(("Pewter Pokémon Center", 7, 4)),         // 0x3a
    Some(("Mt Moon 1F", 20, 18)),                  // 0x3b
    Some(("Mt Moon B1F", 14, 14)),                 // 0x3c
    Some(("Mt Moon B2F", 20, 18)),                 // 0x3d
    Some(("Cerulean Trashed House", 4, 4)),        // 0x3e
    Some(("Cerulean Trade House", 4, 4)),          // 0x3f
    Some(("Cerulean Pokémon Center", 7, 4)),       // 0x40
    Some(("Cerulean Gym", 5, 7)),                  // 0x41
    Some(("Bike Shop", 4, 4)),                     // 0x42
    Some(("Cerulean Mart", 4, 4)),                 // 0x43
    Some(("Mt Moon Pokémon Center", 7, 4)),        // 0x44
    Some(("Cerulean Trashed House Copy", 4, 4)),   // 0x45
    Some(("Route 5 Gate", 4, 3)),                  // 0x46
    Some(("Underground Path Route 5", 4, 4)),      // 0x47
    Some(("Day Care", 4, 4)),                      // 0x48
    Some(("Route 6 Gate", 4, 3)),                  // 0x49
    Some(("Underground Path Route 6", 4, 4)),      // 0x4a
    Some(("Underground Path Route 6 Copy", 4, 4)), // 0x4b
    Some(("Route 7 Gate", 3, 4)),                  // 0x4c
    Some(("Underground Path Route 7", 4, 4)),      // 0x4d
    Some(("Underground Path Route 7 Copy", 4, 4)), // 0x4e
    Some(("Route 8 Gate", 3, 4)),                  // 0x4f
    Some(("Underground Path Route 8", 4, 4)),      // 0x50
    Some(("Rock Tunnel Pokémon Center", 7, 4)),    // 0x51
    Some(("Rock Tunnel 1F", 20, 18)),              // 0x52
    Some(("Power Plant", 20, 18)),                 // 0x53
    Some(("Route 11 Gate 1F", 4, 5)),              // 0x54
    Some(("Digletts Cave Route 11", 4, 4)),        // 0x55
    Some(("Route 11 Gate 2F", 4, 4)),              // 0x56
    Some(("Route 12 Gate 1F", 5, 4)),              // 0x57
    Some(("Bill's House", 4, 4)),                  // 0x58
    Some(("Vermilion Pokémon Center", 7, 4)),      // 0x59
    Some(("Pokémon Fan Club", 4, 4)),              // 0x5a
    Some(("Vermilion Mart", 4, 4)),                // 0x5b
    Some(("Vermilion Gym", 5, 9)),                 // 0x5c
    Some(("Vermilion Pidgey House", 4, 4)),        // 0x5d
    Some(("Vermilion Dock", 14, 6)),               // 0x5e
    Some(("SS Anne 1F", 20, 9)),                   // 0x5f
    Some(("SS Anne 2F", 20, 9)),                   // 0x60
    Some(("SS Anne 3F", 10, 3)),                   // 0x61
    Some(("SS Anne B1F", 15, 4)),                  // 0x62
    Some(("SS Anne Bow", 10, 7)),                  // 0x63
    Some(("SS Anne Kitchen", 7, 8)),               // 0x64
    Some(("SS Anne Captains Room", 3, 4)),         // 0x65
    Some(("SS Anne 1F Rooms", 12, 8)),             // 0x66
    Some(("SS Anne 2F Rooms", 12, 8)),             // 0x67
    Some(("SS Anne B1F Rooms", 12, 8)),            // 0x68
    None,                                          // 0x69
    None,                                          // 0x6a
    None,                                          // 0x6b
    Some(("Victory Road 1F", 10, 9)),              // 0x6c
    None,                                          // 0x6d
    None,                                          // 0x6e
    None,                                          // 0x6f
    None,                                          // 0x70
    Some(("Lances Room", 13, 13)),                 // 0x71
    None,                                          // 0x72
    None,                                          // 0x73
    None,                                          // 0x74
    None,                                          // 0x75
    Some(("Hall Of Fame", 5, 4)),                  // 0x76
    Some(("Underground Path North South", 4, 24)), // 0x77
    Some(("Champions Room", 4, 4)),                // 0x78
    Some(("Underground Path West East", 25, 4)),   // 0x79
    Some(("Celadon Mart 1F", 10, 4)),              // 0x7a
    Some(("Celadon Mart 2F", 10, 4)),              // 0x7b
    Some(("Celadon Mart 3F", 10, 4)),              // 0x7c
    Some(("Celadon Mart 4F", 10, 4)),              // 0x7d
    Some(("Celadon Mart Roof", 10, 4)),            // 0x7e
    Some(("Celadon Mart Elevator", 2, 2)),         // 0x7f
    Some(("Celadon Mansion 1F", 4, 6)),            // 0x80
    Some(("Celadon Mansion 2F", 4, 6)),            // 0x81
    Some(("Celadon Mansion 3F", 4, 6)),            // 0x82
    Some(("Celadon Mansion Roof", 4, 6)),          // 0x83
    Some(("Celadon Mansion Roof House", 4, 4)),    // 0x84
    Some(("Celadon Pokémon Center", 7, 4)),        // 0x85
    Some(("Celadon Gym", 5, 9)),                   // 0x86
    Some(("Game Corner", 10, 9)),                  // 0x87
    Some(("Celadon Mart 5F", 10, 4)),              // 0x88
    Some(("Game Corner Prize Room", 5, 4)),        // 0x89
    Some(("Celadon Diner", 5, 4)),                 // 0x8a
    Some(("Celadon Chief House", 4, 4)),           // 0x8b
    Some(("Celadon Hotel", 7, 4)),                 // 0x8c
    Some(("Lavender Pokémon Center", 7, 4)),       // 0x8d
    Some(("Pokémon Tower 1F", 10, 9)),             // 0x8e
    Some(("Pokémon Tower 2F", 10, 9)),             // 0x8f
    Some(("Pokémon Tower 3F", 10, 9)),             // 0x90
    Some(("Pokémon Tower 4F", 10, 9)),             // 0x91
    Some(("Pokémon Tower 5F", 10, 9)),             // 0x92
    Some(("Pokémon Tower 6F", 10, 9)),             // 0x93
    Some(("Pokémon Tower 7F", 10, 9)),             // 0x94
    Some(("Mr. Fuji's House", 4, 4)),              // 0x95
    Some(("Lavender Mart", 4, 4)),                 // 0x96
    Some(("Lavender Cubone House", 4, 4)),         // 0x97
    Some(("Fuchsia Mart", 4, 4)),                  // 0x98
    Some(("Fuchsia Bill's Grandpas House", 4, 4)), // 0x99
    Some(("Fuchsia Pokémon Center", 7, 4)),        // 0x9a
    Some(("Wardens House", 5, 4)),                 // 0x9b
    Some(("Safari Zone Gate", 4, 3)),              // 0x9c
    Some(("Fuchsia Gym", 5, 9)),                   // 0x9d
    Some(("Fuchsia Meeting Room", 7, 4)),          // 0x9e
    Some(("Seafoam Islands B1F", 15, 9)),          // 0x9f
    Some(("Seafoam Islands B2F", 15, 9)),          // 0xa0
    Some(("Seafoam Islands B3F", 15, 9)),          // 0xa1
    Some(("Seafoam Islands B4F", 15, 9)),          // 0xa2
    Some(("Vermilion Old Rod House", 4, 4)),       // 0xa3
    Some(("Fuchsia Good Rod House", 4, 4)),        // 0xa4
    Some(("Pokémon Mansion 1F", 15, 14)),          // 0xa5
    Some(("Cinnabar Gym", 10, 9)),                 // 0xa6
    Some(("Cinnabar Lab", 9, 4)),                  // 0xa7
    Some(("Cinnabar Lab Trade Room", 4, 4)),       // 0xa8
    Some(("Cinnabar Lab Metronome Room", 4, 4)),   // 0xa9
    Some(("Cinnabar Lab Fossil Room", 4, 4)),      // 0xaa
    Some(("Cinnabar Pokémon Center", 7, 4)),       // 0xab
    Some(("Cinnabar Mart", 4, 4)),                 // 0xac
    Some(("Cinnabar Mart Copy", 4, 4)),            // 0xad
    Some(("Indigo Plateau Lobby", 8, 6)),          // 0xae
    Some(("Copycats House 1F", 4, 4)),             // 0xaf
    Some(("Copycats House 2F", 4, 4)),             // 0xb0
    Some(("Fighting Dojo", 5, 6)),                 // 0xb1
    Some(("Saffron Gym", 10, 9)),                  // 0xb2
    Some(("Saffron Pidgey House", 4, 4)),          // 0xb3
    Some(("Saffron Mart", 4, 4)),                  // 0xb4
    Some(("Silph Co 1F", 15, 9)),                  // 0xb5
    Some(("Saffron Pokémon Center", 7, 4)),        // 0xb6
    Some(("Mr. Psychic's House", 4, 4)),           // 0xb7
    Some(("Route 15 Gate 1F", 4, 5)),              // 0xb8
    Some(("Route 15 Gate 2F", 4, 4)),              // 0xb9
    Some(("Route 16 Gate 1F", 4, 7)),              // 0xba
    Some(("Route 16 Gate 2F", 4, 4)),              // 0xbb
    Some(("Route 16 Fly House", 4, 4)),            // 0xbc
    Some(("Route 12 Super Rod House", 4, 4)),      // 0xbd
    Some(("Route 18 Gate 1F", 4, 5)),              // 0xbe
    Some(("Route 18 Gate 2F", 4, 4)),              // 0xbf
    Some(("Seafoam Islands 1F", 15, 9)),           // 0xc0
    Some(("Route 22 Gate", 5, 4)),                 // 0xc1
    Some(("Victory Road 2F", 15, 9)),              // 0xc2
    Some(("Route 12 Gate 2F", 4, 4)),              // 0xc3
    Some(("Vermilion Trade House", 4, 4)),         // 0xc4
    Some(("Digletts Cave", 20, 18)),               // 0xc5
    Some(("Victory Road 3F", 15, 9)),              // 0xc6
    Some(("Rocket Hideout B1F", 15, 14)),          // 0xc7
    Some(("Rocket Hideout B2F", 15, 14)),          // 0xc8
    Some(("Rocket Hideout B3F", 15, 14)),          // 0xc9
    Some(("Rocket Hideout B4F", 15, 12)),          // 0xca
    Some(("Rocket Hideout Elevator", 3, 4)),       // 0xcb
    None,                                          // 0xcc
    None,                                          // 0xcd
    None,                                          // 0xce
    Some(("Silph Co 2F", 15, 9)),                  // 0xcf
    Some(("Silph Co 3F", 15, 9)),                  // 0xd0
    Some(("Silph Co 4F", 15, 9)),                  // 0xd1
    Some(("Silph Co 5F", 15, 9)),                  // 0xd2
    Some(("Silph Co 6F", 13, 9)),                  // 0xd3
    Some(("Silph Co 7F", 13, 9)),                  // 0xd4
    Some(("Silph Co 8F", 13, 9)),                  // 0xd5
    Some(("Pokémon Mansion 2F", 15, 14)),          // 0xd6
    Some(("Pokémon Mansion 3F", 15, 9)),           // 0xd7
    Some(("Pokémon Mansion B1F", 15, 14)),         // 0xd8
    Some(("Safari Zone East", 15, 13)),            // 0xd9
    Some(("Safari Zone North", 20, 18)),           // 0xda
    Some(("Safari Zone West", 15, 13)),            // 0xdb
    Some(("Safari Zone Center", 15, 13)),          // 0xdc
    Some(("Safari Zone Center Rest House", 4, 4)), // 0xdd
    Some(("Safari Zone Secret House", 4, 4)),      // 0xde
    Some(("Safari Zone West Rest House", 4, 4)),   // 0xdf
    Some(("Safari Zone East Rest House", 4, 4)),   // 0xe0
    Some(("Safari Zone North Rest House", 4, 4)),  // 0xe1
    Some(("Cerulean Cave 2F", 15, 9)),             // 0xe2
    Some(("Cerulean Cave B1F", 15, 9)),            // 0xe3
    Some(("Cerulean Cave 1F", 15, 9)),             // 0xe4
    Some(("Name Raters House", 4, 4)),             // 0xe5
    Some(("Cerulean Badge House", 4, 4)),          // 0xe6
    None,                                          // 0xe7
    Some(("Rock Tunnel B1F", 20, 18)),             // 0xe8
    Some(("Silph Co 9F", 13, 9)),                  // 0xe9
    Some(("Silph Co 10F", 8, 9)),                  // 0xea
    Some(("Silph Co 11F", 9, 9)),                  // 0xeb
    Some(("Silph Co Elevator", 2, 2)),             // 0xec
    None,                                          // 0xed
    None,                                          // 0xee
    Some(("Trade Center", 5, 4)),                  // 0xef
    Some(("Colosseum", 5, 4)),                     // 0xf0
    None,                                          // 0xf1
    None,                                          // 0xf2
    None,                                          // 0xf3
    None,                                          // 0xf4
    Some(("Loreleis Room", 5, 6)),                 // 0xf5
    Some(("Brunos Room", 5, 6)),                   // 0xf6
    Some(("Agathas Room", 5, 6)),                  // 0xf7
    None,                                          // 0xf8
    None,                                          // 0xf9
    None,                                          // 0xfa
    None,                                          // 0xfb
    None,                                          // 0xfc
    None,                                          // 0xfd
    None,                                          // 0xfe
    None,                                          // 0xff
];
