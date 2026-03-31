use crate::sync::{pull_events, push_new_events, save_sync_state, SyncState};
use app_core::events::{Event, EventEnvelope};
use app_core::models::Role;

pub struct PulledAssistantScoreEvents {
    pub assistant_score_events: Vec<Event>,
    pub new_server_events_seen: usize,
    pub updated_sync_state: SyncState,
}

pub async fn pull_assistant_score_events(
    mut sync_state: SyncState,
    local_events: &[EventEnvelope],
) -> Result<PulledAssistantScoreEvents, String> {
    let baseline_server_cursor = sync_state.last_pushed_seq;

    // Push first so the server stays the canonical cross-device timeline before
    // we ask for assistant-entered deltas.
    push_new_events(&mut sync_state, local_events).await?;

    let mut since = baseline_server_cursor as u32;
    let mut assistant_score_events = Vec::new();
    let mut new_server_events_seen = 0usize;

    loop {
        let response = pull_events(&sync_state.session_id, since).await?;
        if response.events.is_empty() {
            sync_state.last_pushed_seq = sync_state.last_pushed_seq.max(response.cursor);
            save_sync_state(&sync_state);
            break;
        }

        new_server_events_seen += response.events.len();
        for envelope in response.events {
            if envelope.entered_by != Role::Assistant {
                continue;
            }
            if let Event::ScoreEntered { .. } = &envelope.payload {
                assistant_score_events.push(envelope.payload);
            }
        }

        since = response.cursor as u32;
        sync_state.last_pushed_seq = sync_state.last_pushed_seq.max(response.cursor);
    }

    Ok(PulledAssistantScoreEvents {
        assistant_score_events,
        new_server_events_seen,
        updated_sync_state: sync_state,
    })
}
