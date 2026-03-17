use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecorationInteractionSnapshot {
    pub hovered_ids: Vec<String>,
    pub active_ids: Vec<String>,
}

impl DecorationInteractionSnapshot {
    pub fn is_hovered(&self, id: &str) -> bool {
        self.hovered_ids.iter().any(|candidate| candidate == id)
    }

    pub fn is_active(&self, id: &str) -> bool {
        self.active_ids.iter().any(|candidate| candidate == id)
    }
}

#[cfg(test)]
mod tests {
    use super::DecorationInteractionSnapshot;

    #[test]
    fn interaction_snapshot_tracks_hover_and_active_ids() {
        let snapshot = DecorationInteractionSnapshot {
            hovered_ids: vec!["close".into()],
            active_ids: vec!["maximize".into()],
        };

        assert!(snapshot.is_hovered("close"));
        assert!(!snapshot.is_hovered("maximize"));
        assert!(snapshot.is_active("maximize"));
        assert!(!snapshot.is_active("close"));
    }
}
