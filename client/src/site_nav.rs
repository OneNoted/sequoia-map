//! The seqwawa.com navigation tree, as shown in the map's site navbar.
//!
//! SOURCE OF TRUTH: `sequoia-web/frontend/src/lib/site/nav.ts` (`SECTIONS`) and
//! `sequoia-web/frontend/src/lib/icons.ts`. This is a deliberate hand-synced
//! copy across repos - the map cannot import from the SvelteKit app - so when a
//! section is added or renamed on the website, mirror it here. Only the four
//! fields the bar renders are carried over; the page-scaffolding fields
//! (kicker/title/lead/placeholder) stay on the website.
//!
//! `SECTIONS` is the whole registry; the bar draws [`visible_sections`], which
//! applies the website's default menu (`navPreferences.ts`) on top of it. A
//! viewer's own choice lives in the `seq_nav_hidden` cookie, which is HttpOnly
//! and host-only to seqwawa.com, so the map cannot read it - it shows what a
//! visitor who never chose sees, which is what nearly everyone sees.

/// A leaf link inside a dropdown.
#[derive(Clone, Copy)]
pub struct NavLeaf {
    pub label: &'static str,
    pub path: &'static str,
    pub icon: &'static str,
    pub description: &'static str,
}

/// A labelled column of leaves inside a dropdown.
#[derive(Clone, Copy)]
pub struct NavGroup {
    pub label: &'static str,
    pub path: &'static str,
    pub icon: &'static str,
    pub items: &'static [NavLeaf],
}

/// A top-level bar entry. It may open a dropdown of groups, of bare items, or
/// neither (a plain link).
#[derive(Clone, Copy)]
pub struct NavSection {
    pub label: &'static str,
    pub path: &'static str,
    pub groups: &'static [NavGroup],
    pub items: &'static [NavLeaf],
}

/// A section as the bar should draw it: the registry with the default hidden
/// entries removed, and the category headings already resolved away when the
/// menu is short enough to read as one list.
#[derive(Clone)]
pub struct MenuSection {
    pub label: &'static str,
    pub path: &'static str,
    pub groups: Vec<MenuGroup>,
    pub items: Vec<NavLeaf>,
}

/// A labelled column of a resolved menu.
#[derive(Clone)]
pub struct MenuGroup {
    pub label: &'static str,
    pub path: &'static str,
    pub icon: &'static str,
    pub items: Vec<NavLeaf>,
}

impl MenuSection {
    pub fn has_menu(&self) -> bool {
        !self.groups.is_empty() || !self.items.is_empty()
    }
}

/// Entries the website keeps out of the bar until a viewer opts back in.
/// Mirrors `DEFAULT_HIDDEN_NAV` in `navPreferences.ts`; every page stays
/// reachable by URL, this only trims the menu to the destinations most
/// visitors actually use.
const DEFAULT_HIDDEN: &[&str] = &[
    "/news",
    "/statistics/player/raids",
    "/statistics/player/activity",
    "/statistics/guilds/war",
    "/statistics/global/rankings",
    "/statistics/global/guilds",
    "/statistics/global/players",
    "/services/announcements",
];

/// A menu stacks into one column until it carries more entries than this. Past
/// it the category headings come back on their own, so a longer list stays
/// scannable and the columns stay a sensible height.
const FLAT_MENU_MAX_ITEMS: usize = 3;

/// The bar's view of the site tree, shaped the way the menu should draw it: a
/// section comes back with `groups` when its menu earns category headings and
/// columns, and with a flat `items` list when it does not.
///
/// Category headings default to off on the website, so a grouped menu only
/// keeps them once it grows past [`FLAT_MENU_MAX_ITEMS`] leaves.
pub fn visible_sections() -> Vec<MenuSection> {
    SECTIONS
        .iter()
        .filter(|section| !is_hidden(section.path))
        .map(resolve_section)
        .collect()
}

fn is_hidden(path: &str) -> bool {
    DEFAULT_HIDDEN.contains(&path)
}

fn visible_leaves(items: &[NavLeaf]) -> Vec<NavLeaf> {
    items
        .iter()
        .copied()
        .filter(|leaf| !is_hidden(leaf.path))
        .collect()
}

fn resolve_section(section: &NavSection) -> MenuSection {
    let items = visible_leaves(section.items);
    // A group disappears once every entry under it is hidden.
    let groups: Vec<MenuGroup> = section
        .groups
        .iter()
        .map(|group| MenuGroup {
            label: group.label,
            path: group.path,
            icon: group.icon,
            items: visible_leaves(group.items),
        })
        .filter(|group| !group.items.is_empty())
        .collect();

    let leaf_count: usize =
        items.len() + groups.iter().map(|group| group.items.len()).sum::<usize>();
    if !groups.is_empty() && leaf_count <= FLAT_MENU_MAX_ITEMS {
        // Collapse into one stacked column, in registry order.
        return MenuSection {
            label: section.label,
            path: section.path,
            groups: Vec::new(),
            // Bare leaves on the section keep their registry position ahead of
            // the grouped ones; dropping them here would silently lose a menu
            // entry the next time nav.ts is mirrored over.
            items: items
                .into_iter()
                .chain(groups.into_iter().flat_map(|group| group.items))
                .collect(),
        };
    }

    MenuSection {
        label: section.label,
        path: section.path,
        groups,
        items,
    }
}

pub const SECTIONS: &[NavSection] = &[
    NavSection {
        label: "News",
        path: "/news",
        groups: &[],
        items: &[],
    },
    NavSection {
        label: "Events",
        path: "/events",
        groups: &[],
        items: &[],
    },
    NavSection {
        label: "Statistics",
        path: "/statistics",
        groups: &[
            NavGroup {
                label: "Player",
                path: "/statistics/player",
                icon: ICON_USERS,
                items: &[
                    NavLeaf {
                        label: "Raids",
                        path: "/statistics/player/raids",
                        icon: ICON_RAID,
                        description: "Completions by raid.",
                    },
                    NavLeaf {
                        label: "Activity",
                        path: "/statistics/player/activity",
                        icon: ICON_ONLINE,
                        description: "Presence and gains over time.",
                    },
                    NavLeaf {
                        label: "Playercard",
                        path: "/statistics/player/playercard",
                        icon: ICON_CLIPBOARD,
                        description: "Member profiles.",
                    },
                ],
            },
            NavGroup {
                label: "Guilds",
                path: "/statistics/guilds",
                icon: ICON_TROPHY,
                items: &[
                    NavLeaf {
                        label: "War",
                        path: "/statistics/guilds/war",
                        icon: ICON_WAR,
                        description: "War leaderboards.",
                    },
                    NavLeaf {
                        label: "Raid",
                        path: "/statistics/guilds/raid",
                        icon: ICON_SWORD,
                        description: "Raid output and events.",
                    },
                ],
            },
            NavGroup {
                label: "Global",
                path: "/statistics/global",
                icon: ICON_MAP,
                items: &[
                    NavLeaf {
                        label: "Rankings",
                        path: "/statistics/global/rankings",
                        icon: ICON_TROPHY,
                        description: "Where every guild stands.",
                    },
                    NavLeaf {
                        label: "Guilds",
                        path: "/statistics/global/guilds",
                        icon: ICON_TERRITORY,
                        description: "Browse or look up any guild.",
                    },
                    NavLeaf {
                        label: "Players",
                        path: "/statistics/global/players",
                        icon: ICON_USERS,
                        description: "Player rankings and lookup.",
                    },
                ],
            },
        ],
        items: &[],
    },
    NavSection {
        label: "Services",
        path: "/services",
        groups: &[],
        items: &[
            NavLeaf {
                label: "Bagshop",
                path: "/services/bagshop",
                icon: ICON_BAG,
                description: "Marketplace for raid crafterbags.",
            },
            NavLeaf {
                label: "Announcements",
                path: "/services/announcements",
                icon: ICON_DOCUMENT,
                description: "Guild-internal announcements for members.",
            },
            NavLeaf {
                label: "Requests",
                path: "/services/requests",
                icon: ICON_CLIPBOARD,
                description: "Request Aspects and Guild Tomes here.",
            },
        ],
    },
];

// SVG path data, copied from `sequoia-web/frontend/src/lib/icons.ts`. Drawn as
// stroked 24x24 outlines, matching the website's `Icon.svelte`.
pub const ICON_MAP: &str = "M9 20l-5.447-2.724A1 1 0 013 16.382V5.618a1 1 0 011.447-.894L9 7m0 13l6-3m-6 3V7m6 10l4.553 2.276A1 1 0 0021 18.382V7.618a1 1 0 00-.553-.894L15 4m0 13V4m0 0L9 7";
pub const ICON_USERS: &str = "M17 20h5v-2a3 3 0 00-5.356-1.857M17 20H7m10 0v-2c0-.656-.126-1.283-.356-1.857M7 20H2v-2a3 3 0 015.356-1.857M7 20v-2c0-.656.126-1.283.356-1.857m0 0a5.002 5.002 0 019.288 0M15 7a3 3 0 11-6 0 3 3 0 016 0z";
pub const ICON_TERRITORY: &str = "M3 21V3h18v18H3zm2-2h14V5H5v14z";
pub const ICON_WAR: &str = "M5 4l14 16M19 4L5 20M6 16.5h4M14 16.5h4";
pub const ICON_RAID: &str =
    "M12 2L4 7v6c0 5.25 3.4 10.1 8 11.2 4.6-1.1 8-5.95 8-11.2V7l-8-5zM12 8v5M12 16h.01";
pub const ICON_ONLINE: &str =
    "M5.636 18.364a9 9 0 010-12.728m12.728 0a9 9 0 010 12.728M12 12a1 1 0 110-2 1 1 0 010 2z";
pub const ICON_TROPHY: &str =
    "M8 21h8M12 17v4M7 4h10v4a5 5 0 01-10 0V4zM7 6H4v1a3 3 0 003 3M17 6h3v1a3 3 0 01-3 3";
pub const ICON_BAG: &str = "M6 7h12l1 13a1 1 0 01-1 1H6a1 1 0 01-1-1L6 7zM9 7V5a3 3 0 016 0v2";
pub const ICON_DOCUMENT: &str =
    "M7 3h7l5 5v13a1 1 0 01-1 1H7a1 1 0 01-1-1V4a1 1 0 011-1zm7 0v5h5M9 13h6M9 17h6";
pub const ICON_CLIPBOARD: &str = "M9 4h6a1 1 0 011 1v1h2a1 1 0 011 1v13a1 1 0 01-1 1H6a1 1 0 01-1-1V7a1 1 0 011-1h2V5a1 1 0 011-1zm0 2h6M9 12h6M9 16h4";
pub const ICON_SWORD: &str = "M14.5 3.5l6 6-9 9-2 .5-1 1-3-3 1-1 .5-2 9-9zM7 17l-3 3M9 14l3 3";

/// The Discord wordmark glyph, drawn filled rather than stroked.
pub const ICON_DISCORD: &str = "M20.317 4.37a19.791 19.791 0 00-4.885-1.515.074.074 0 00-.079.037c-.21.375-.444.864-.608 1.25a18.27 18.27 0 00-5.487 0 12.64 12.64 0 00-.617-1.25.077.077 0 00-.079-.037A19.736 19.736 0 003.677 4.37a.07.07 0 00-.032.027C.533 9.046-.32 13.58.099 18.057a.082.082 0 00.031.057 19.9 19.9 0 005.993 3.03.078.078 0 00.084-.028c.462-.63.874-1.295 1.226-1.994a.076.076 0 00-.041-.106 13.107 13.107 0 01-1.872-.892.077.077 0 01-.008-.128 10.2 10.2 0 00.372-.292.074.074 0 01.077-.01c3.928 1.793 8.18 1.793 12.062 0a.074.074 0 01.078.01c.12.098.246.198.373.292a.077.077 0 01-.006.127 12.299 12.299 0 01-1.873.892.077.077 0 00-.041.107c.36.698.772 1.362 1.225 1.993a.076.076 0 00.084.028 19.839 19.839 0 006.002-3.03.077.077 0 00.032-.054c.5-5.177-.838-9.674-3.549-13.66a.061.061 0 00-.031-.03z";

pub const DISCORD_URL: &str = "https://discord.gg/seq";

#[cfg(test)]
mod tests {
    use super::*;

    fn section(path: &str) -> Option<MenuSection> {
        visible_sections()
            .into_iter()
            .find(|section| section.path == path)
    }

    fn labels(items: &[NavLeaf]) -> Vec<&'static str> {
        items.iter().map(|leaf| leaf.label).collect()
    }

    #[test]
    fn hidden_sections_leave_the_bar() {
        assert!(section("/news").is_none());
        assert!(section("/events").is_some());
    }

    #[test]
    fn a_short_grouped_menu_flattens_into_one_column() {
        // Statistics keeps one entry under Player and one under Guilds; Global
        // empties out entirely. Two leaves is under the flattening threshold,
        // so the category headings go.
        let statistics = section("/statistics").expect("statistics stays in the bar");
        assert!(statistics.groups.is_empty());
        assert_eq!(labels(&statistics.items), ["Playercard", "Raid"]);
    }

    #[test]
    fn a_long_grouped_menu_keeps_its_headings() {
        let full = resolve_section(&NavSection {
            label: "Statistics",
            path: "/statistics",
            groups: &[
                NavGroup {
                    label: "Player",
                    path: "/statistics/player",
                    icon: ICON_USERS,
                    items: &[
                        NavLeaf {
                            label: "Playercard",
                            path: "/statistics/player/playercard",
                            icon: ICON_CLIPBOARD,
                            description: "Member profiles.",
                        },
                        NavLeaf {
                            label: "Raid",
                            path: "/statistics/guilds/raid",
                            icon: ICON_SWORD,
                            description: "Raid output and events.",
                        },
                    ],
                },
                NavGroup {
                    label: "Guilds",
                    path: "/statistics/guilds",
                    icon: ICON_TROPHY,
                    items: &[
                        NavLeaf {
                            label: "Bagshop",
                            path: "/services/bagshop",
                            icon: ICON_BAG,
                            description: "Marketplace for raid crafterbags.",
                        },
                        NavLeaf {
                            label: "Requests",
                            path: "/services/requests",
                            icon: ICON_CLIPBOARD,
                            description: "Request Aspects and Guild Tomes here.",
                        },
                    ],
                },
            ],
            items: &[],
        });

        assert_eq!(full.groups.len(), 2);
        assert!(full.items.is_empty());
    }

    #[test]
    fn a_flattened_menu_keeps_the_sections_own_leaves() {
        let mixed = resolve_section(&NavSection {
            label: "Services",
            path: "/services",
            groups: &[NavGroup {
                label: "Guilds",
                path: "/statistics/guilds",
                icon: ICON_TROPHY,
                items: &[NavLeaf {
                    label: "Bagshop",
                    path: "/services/bagshop",
                    icon: ICON_BAG,
                    description: "Marketplace for raid crafterbags.",
                }],
            }],
            items: &[NavLeaf {
                label: "Requests",
                path: "/services/requests",
                icon: ICON_CLIPBOARD,
                description: "Request Aspects and Guild Tomes here.",
            }],
        });

        assert!(mixed.groups.is_empty());
        assert_eq!(labels(&mixed.items), ["Requests", "Bagshop"]);
    }

    #[test]
    fn a_flat_menu_only_drops_its_hidden_entries() {
        let services = section("/services").expect("services stays in the bar");
        assert!(services.groups.is_empty());
        assert_eq!(labels(&services.items), ["Bagshop", "Requests"]);
    }

    #[test]
    fn a_section_without_a_menu_stays_a_plain_link() {
        let events = section("/events").expect("events stays in the bar");
        assert!(!events.has_menu());
    }
}
