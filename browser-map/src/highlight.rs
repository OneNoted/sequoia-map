//! Which territory instances the hover and the selection point at.

use std::collections::HashMap;

/// The instance index the shader reads as "none".
pub(crate) const NO_INSTANCE: i32 = -1;

/// Each territory's position in the instance buffer, from the last instance build. The hovered
/// and selected territories reach the shader as two indices into it, so changing either needs
/// no rebuild of the instances, only of this lookup when the instances themselves are rebuilt.
#[derive(Debug, Default)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) struct InstanceIndex(HashMap<String, i32>);

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
impl InstanceIndex {
    /// Records `names` in instance order.
    pub(crate) fn rebuild<'a>(&mut self, names: impl IntoIterator<Item = &'a String>) {
        self.0.clear();
        self.0.extend(
            names
                .into_iter()
                .enumerate()
                .map(|(index, name)| (name.clone(), index as i32)),
        );
    }

    /// The instance index of `name`, or [`NO_INSTANCE`] when there is none or it is not drawn.
    pub(crate) fn of(&self, name: Option<&str>) -> i32 {
        name.and_then(|name| self.0.get(name).copied())
            .unwrap_or(NO_INSTANCE)
    }
}

#[cfg(test)]
mod tests {
    use super::{InstanceIndex, NO_INSTANCE};

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn names_map_to_their_instance_order() {
        let mut index = InstanceIndex::default();
        index.rebuild(&names(&["Ragni", "Detlas", "Almuj"]));
        assert_eq!(index.of(Some("Ragni")), 0);
        assert_eq!(index.of(Some("Almuj")), 2);
        assert_eq!(index.of(Some("Nemract")), NO_INSTANCE);
        assert_eq!(index.of(None), NO_INSTANCE);
    }

    #[test]
    fn a_rebuild_follows_reordered_and_replaced_territories() {
        let mut index = InstanceIndex::default();
        index.rebuild(&names(&["Ragni", "Detlas", "Almuj"]));
        index.rebuild(&names(&["Almuj", "Nemract", "Ragni"]));
        assert_eq!(index.of(Some("Almuj")), 0);
        assert_eq!(index.of(Some("Ragni")), 2);
        assert_eq!(index.of(Some("Nemract")), 1);
        assert_eq!(index.of(Some("Detlas")), NO_INSTANCE);
        index.rebuild(&names(&[]));
        assert_eq!(index.of(Some("Almuj")), NO_INSTANCE);
    }

    #[test]
    fn a_selection_whose_territory_is_gone_selects_no_other() {
        let mut index = InstanceIndex::default();
        assert_eq!(index.of(Some("Ragni")), NO_INSTANCE);
        index.rebuild(&names(&["Ragni"]));
        assert_eq!(index.of(Some("Ragni")), 0);
        // Ragni replaced by Detlas at the same position: the old 0 must not carry over.
        index.rebuild(&names(&["Detlas"]));
        assert_eq!(index.of(Some("Ragni")), NO_INSTANCE);
        assert_eq!(index.of(Some("Detlas")), 0);
    }
}
