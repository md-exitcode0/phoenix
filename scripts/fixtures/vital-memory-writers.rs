// Included inside the extracted Canvas module by check-vital-memory-writers.py.
// Both the tool and Canvas mutation/read functions are actual production source.
#[cfg(test)]
mod cross_writer_tests {
    use super::*;
    use crate::tools::vital_memory::{execute, VitalMemoryWriteInput};

    fn save(note: &str, replaces: Option<&str>) -> anyhow::Result<crate::tools::ToolOutput> {
        execute(VitalMemoryWriteInput {
            note: note.into(),
            category: "preference".into(),
            replaces: replaces.map(str::to_string),
        })
    }

    #[test]
    fn tool_correction_ui_add_delete_then_tool_correction_preserves_history() {
        let path = vitals_path();
        std::fs::write(&path, "").unwrap();
        save("Prefers tea", None).unwrap();
        save("Prefers coffee", Some("Prefers tea")).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let first =
            serde_json::to_value(vital_memory_document::split_history(&raw).unwrap().1).unwrap();

        let added = vitals_write_at(&path, "Finish the preview".into(), "goal".into()).unwrap();
        assert_eq!(vitals_list_blocking().unwrap().as_array().unwrap().len(), 2);
        vitals_delete_at(
            &path,
            Some(added["id"].as_str().unwrap().into()),
            None,
            None,
        )
        .unwrap();
        vitals_write_at(
            &path,
            "Use keyboard navigation".into(),
            "instruction".into(),
        )
        .unwrap();
        vitals_delete_at(
            &path,
            None,
            Some("instruction".into()),
            Some("Use keyboard navigation".into()),
        )
        .unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            serde_json::to_value(vital_memory_document::split_history(&raw).unwrap().1).unwrap(),
            first
        );

        save("Prefers water", Some("Prefers coffee")).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let (active, history) = vital_memory_document::split_history(&raw).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(serde_json::to_value(&history[0]).unwrap(), first[0]);
        assert_eq!(history[1].previous_note, "Prefers coffee");
        assert!(active.contains("Prefers water"));
        for stale in [
            "Prefers tea",
            "Prefers coffee",
            "Finish the preview",
            "Use keyboard navigation",
        ] {
            assert!(!active.contains(stale));
            assert!(!crate::tools::vital_memory::context_block()
                .unwrap()
                .contains(stale));
        }
        let before = std::fs::read(&path).unwrap();
        assert!(save("Must not resurrect tea", Some("Prefers tea")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        vitals_delete_at(
            &path,
            None,
            Some("preference".into()),
            Some("Prefers water".into()),
        )
        .unwrap();
        let deleted = std::fs::read(&path).unwrap();
        assert_eq!(
            vital_memory_document::split_history(std::str::from_utf8(&deleted).unwrap())
                .unwrap()
                .1
                .len(),
            2
        );
        assert!(crate::tools::vital_memory::context_block().is_none());
        assert!(save("Must not resurrect the deleted fact", Some("Prefers water")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), deleted);
    }

    #[test]
    fn legacy_correction_cannot_publish_history_that_the_next_reader_rejects() {
        let path = vitals_path();
        let legacy_note = "x".repeat(241);
        let original = format!(
            "## Preferences\n- {legacy_note}\n\n## Always do\n- Ask before sending messages\n"
        );
        std::fs::write(&path, &original).unwrap();
        let error = save("Short corrected fact", Some(&legacy_note))
            .err()
            .expect("invalid revision must fail before publishing");
        assert!(error.to_string().contains("revision fields"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert_eq!(vitals_list_blocking().unwrap().as_array().unwrap().len(), 2);
        let context = crate::tools::vital_memory::context_block().unwrap();
        assert!(context.contains("Ask before sending messages"));
        assert!(!context.contains("INTEGRITY FAILURE"));
    }

    #[test]
    fn literal_marker_notes_round_trip_through_tool_and_canvas_writers() {
        let path = vitals_path();
        let legacy = "## Preferences\n- Old note mentions <!-- phoenix-vital-revisions-v1 literally\n- Later fact must survive\n\n## Always do\n- Ask before sending messages\n";
        std::fs::write(&path, legacy).unwrap();
        assert_eq!(
            vital_memory_document::split_history(legacy).unwrap().0,
            legacy
        );
        let literal = "<!-- phoenix-vital-revisions-v1";
        save(literal, None).unwrap();
        let ui_note = "Canvas explains <!-- phoenix-vital-revisions literally";
        let ui_row = vitals_write_at(&path, ui_note.into(), "goal".into()).unwrap();
        let rows = vitals_list_blocking().unwrap();
        for note in [
            literal,
            ui_note,
            "Later fact must survive",
            "Ask before sending messages",
        ] {
            assert!(rows
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["note"] == note));
            assert!(crate::tools::vital_memory::context_block()
                .unwrap()
                .contains(note));
        }
        assert!(!crate::tools::vital_memory::context_block()
            .unwrap()
            .contains("INTEGRITY FAILURE"));
        save("Updated literal-marker fact", Some(literal)).unwrap();
        vitals_delete_at(
            &path,
            Some(ui_row["id"].as_str().unwrap().into()),
            None,
            None,
        )
        .unwrap();
        vitals_write_at(
            &path,
            "Keep another <!-- phoenix-vital-revisions-v9 example".into(),
            "goal".into(),
        )
        .unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let (active, history) = vital_memory_document::split_history(&raw).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].previous_note, literal);
        assert!(!active.lines().any(|line| line == format!("- {literal}")));
        assert!(!active.contains(ui_note));
        assert!(active.contains("Old note mentions <!-- phoenix-vital-revisions-v1 literally"));
        assert!(active.contains("Later fact must survive"));
        assert!(active.contains("Ask before sending messages"));
        assert!(!crate::tools::vital_memory::context_block()
            .unwrap()
            .contains("INTEGRITY FAILURE"));
    }

    #[test]
    fn every_writer_rejects_damaged_history_without_erasing_standing_instructions() {
        let path = vitals_path();
        let original = format!(
            "## Always do\n- Ask before sending messages\n\n{}not-json\n-->\n",
            vital_memory_document::HISTORY_START
        );
        std::fs::write(&path, &original).unwrap();
        assert!(vitals_list_blocking().unwrap_err().contains("history"));
        assert!(vitals_write_at(&path, "New note".into(), "goal".into()).is_err());
        assert!(vitals_delete_at(
            &path,
            None,
            Some("instruction".into()),
            Some("Ask before sending messages".into())
        )
        .is_err());
        assert!(save("New fact", None).is_err());
        let context = crate::tools::vital_memory::context_block().unwrap();
        assert!(context.contains("Ask before sending messages"));
        assert!(context.contains("VITAL MEMORY INTEGRITY FAILURE"));
        assert!(!context.contains("not-json"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn unknown_revision_metadata_survives_all_writers_and_invalid_fields_fail() {
        let path = vitals_path();
        std::fs::write(&path, "").unwrap();
        save("Old fact", None).unwrap();
        save("Current fact", Some("Old fact")).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let (active, mut revisions) = vital_memory_document::split_history(&raw).unwrap();
        revisions[0].extra.insert(
            "review_reference".into(),
            serde_json::json!({"id":"local-42"}),
        );
        std::fs::write(
            &path,
            vital_memory_document::append_history(active, &revisions).unwrap(),
        )
        .unwrap();
        vitals_write_at(&path, "UI addition".into(), "goal".into()).unwrap();
        vitals_delete_at(&path, None, Some("goal".into()), Some("UI addition".into())).unwrap();
        save("Latest fact", Some("Current fact")).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let (active, mut revisions) = vital_memory_document::split_history(&raw).unwrap();
        assert_eq!(revisions[0].extra["review_reference"]["id"], "local-42");
        revisions[0].corrected_at = "invalid timestamp".into();
        assert!(vital_memory_document::append_history(active, &revisions).is_err());
        // Simulate external damage without asking the validated writer to
        // manufacture an invalid document.
        let damaged = format!(
            "{active}{}{}\n-->\n",
            vital_memory_document::HISTORY_START,
            serde_json::to_string(&revisions).unwrap()
        );
        std::fs::write(&path, &damaged).unwrap();
        assert!(vitals_write_at(&path, "Blocked".into(), "goal".into()).is_err());
        assert!(save("Blocked", None).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), damaged);
    }

    #[test]
    fn canvas_enforces_one_line_and_combined_file_limit_before_publish() {
        let path = vitals_path();
        std::fs::write(&path, "").unwrap();
        assert!(vitals_write_at(&path, "One note\n- Injected note".into(), "goal".into()).is_err());
        save("Old fact", None).unwrap();
        save("Current fact", Some("Old fact")).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let (active, mut revisions) = vital_memory_document::split_history(&raw).unwrap();
        let overhead = raw.len();
        revisions[0].extra.insert(
            "padding".into(),
            serde_json::json!("x".repeat(VITALS_MAX_BYTES - overhead - 40)),
        );
        std::fs::write(
            &path,
            vital_memory_document::append_history(active, &revisions).unwrap(),
        )
        .unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(vitals_write_at(&path, "x".repeat(240), "goal".into())
            .unwrap_err()
            .contains("exceed"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
