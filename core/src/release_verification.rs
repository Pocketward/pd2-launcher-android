//! Cross-surface verification that complements module-local fixtures.

use std::fs;

use tempfile::TempDir;

use crate::{
    diagnostics::{DiagnosticStore, JobKind, JobStatus},
    host_execution::{ProcessOutput, ProcessRequest},
    install::InstallationAuthorization,
    operation::WorkPhase,
    persistence::{LauncherSettings, SettingsStore},
    presentation::{
        AuthorizationInput, AuthorizationView, DomainEvent, Intent, SequencedEvent, UiSnapshot,
    },
};

const BASE_KEY_CANARY: &str = "D2-SECRET-CANARY-1234";
const EXPANSION_KEY_CANARY: &str = "LOD-SECRET-CANARY-5678";

#[test]
fn product_key_canaries_do_not_cross_any_launcher_retained_or_rendered_surface() {
    let fixture = TempDir::new().expect("fixture");
    let settings_path = fixture.path().join("config/settings.json");
    let history_path = fixture.path().join("state/diagnostics/recent.json");
    let export_root = fixture.path().join("state/exports");

    let input = AuthorizationInput::new(
        BASE_KEY_CANARY.to_owned(),
        EXPANSION_KEY_CANARY.to_owned(),
        true,
    )
    .expect("valid authorization fixture");
    let input_debug = format!("{input:?}");
    let intent = Intent::AuthorizeAndInstall(input);
    let history_label = intent.history_label();
    let (base, expansion, consent) = match intent {
        Intent::AuthorizeAndInstall(input) => input.into_parts(),
        _ => unreachable!("fixture intent is authorization"),
    };
    let authorization =
        InstallationAuthorization::new(base, expansion, consent).expect("authorization");
    let authorization_debug = format!("{authorization:?}");

    SettingsStore::new(&settings_path)
        .save(&LauncherSettings::default())
        .expect("settings save");
    let mut diagnostics = DiagnosticStore::open(&history_path, &export_root).expect("diagnostics");
    let job = diagnostics
        .start_job(JobKind::Install, WorkPhase::BaseGameInstall)
        .expect("diagnostic active job");
    diagnostics
        .finish_job(job, JobStatus::Failed)
        .expect("diagnostic completion");
    let export_path = diagnostics.export().expect("diagnostic export");

    let event = SequencedEvent::new(
        1,
        DomainEvent::AuthorizationChanged(AuthorizationView::Authorized),
    );
    let snapshot = UiSnapshot::initial().with_authorization(AuthorizationView::Authorized);
    let process = ProcessRequest::new(format!("/managed/{BASE_KEY_CANARY}"))
        .with_argument(format!("--key={BASE_KEY_CANARY}"))
        .with_environment("KEY_CANARY", EXPANSION_KEY_CANARY);
    let output = ProcessOutput::new(
        BASE_KEY_CANARY.as_bytes().to_vec(),
        EXPANSION_KEY_CANARY.as_bytes().to_vec(),
        false,
        false,
    );

    let retained_and_rendered = [
        ("authorization input debug", input_debug),
        ("intent history", history_label.to_owned()),
        ("installation authorization debug", authorization_debug),
        (
            "durable settings",
            fs::read_to_string(&settings_path).expect("settings"),
        ),
        (
            "diagnostic history",
            fs::read_to_string(&history_path).expect("history"),
        ),
        (
            "diagnostic export",
            fs::read_to_string(export_path).expect("export"),
        ),
        ("structured event", format!("{event:?}")),
        ("UI snapshot/history", format!("{snapshot:?}")),
        ("rendered command", format!("{process:?}")),
        ("retained process output debug", format!("{output:?}")),
    ];

    for (surface, rendered) in retained_and_rendered {
        assert_canaries_absent(surface, &rendered);
    }
}

fn assert_canaries_absent(surface: &str, rendered: &str) {
    for canary in [BASE_KEY_CANARY, EXPANSION_KEY_CANARY] {
        assert!(
            !rendered.contains(canary),
            "product key canary crossed {surface}: {rendered}"
        );
    }
}
