use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf, process::Command};
use teamup_shift_sync_core::{DestinationSnapshot, PlanningConfig, SourceShift, SyncPlan};
use teamup_shift_sync_gui::preview::build_week;

#[derive(Deserialize)]
struct Case {
    config: PlanningConfig,
    names: BTreeMap<String, String>,
    colors: BTreeMap<String, String>,
    shifts: Vec<SourceShift>,
    plan: SyncPlan,
    destination: DestinationSnapshot,
    destination_read: bool,
    expected: Value,
}

#[test]
fn native_week_matches_python_presentation_scenarios() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let python = std::env::var_os("TEAMUP_TEST_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            [
                root.join(".venv/bin/python"),
                root.join(".venv/Scripts/python.exe"),
            ]
            .into_iter()
            .find(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "python" } else { "python3" }))
        });
    let output = Command::new(python)
        .env("PYTHONPATH", root.join("src"))
        .env("PYTHONIOENCODING", "utf-8")
        .arg(root.join("desktop/tests/python_preview.py"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases: Vec<Case> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(cases.len() >= 15);
    for (index, case) in cases.into_iter().enumerate() {
        let week = build_week(
            &case.config,
            &case.names,
            &case.colors,
            &case.shifts,
            &case.plan,
            &case.destination,
            case.destination_read,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&week).unwrap(),
            case.expected,
            "case {index}"
        );
        let mut broken = case.plan.clone();
        if let Some(item) = broken
            .items
            .iter_mut()
            .find(|i| i.step_key == "mithf.create_shift" && i.payload.contains_key("starts_at"))
        {
            item.payload
                .insert("starts_at".into(), Value::String("not-a-time".into()));
            assert!(build_week(
                &case.config,
                &case.names,
                &case.colors,
                &case.shifts,
                &broken,
                &case.destination,
                true
            )
            .is_err());
        }
    }
}
