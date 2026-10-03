//! Stable navigation identities are separate from translated accessibility labels.
use i_slint_backend_testing::{ElementHandle, ElementQuery};

pub fn find(app: &nova::AppWindow, id: &str) -> std::vec::IntoIter<ElementHandle> {
    let id = id.to_owned();
    ElementQuery::from_root(app)
        .match_predicate(move |e| e.accessible_id().is_some_and(|candidate| candidate == id))
        .find_all()
        .into_iter()
}
