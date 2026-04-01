use app_core::models::{Player, PlayerId, PlayerStatus, ScheduledMatch};
use js_sys::{Array, Function, Object, Promise, Reflect};
use std::collections::{HashMap, HashSet};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundScheduleFieldShareCard {
    pub field_label: String,
    pub team_a_player_names: Vec<String>,
    pub team_b_player_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundScheduleShareSnapshot {
    pub session_label: String,
    pub round_number: u32,
    pub scheduled_field_cards: Vec<RoundScheduleFieldShareCard>,
    pub benched_player_names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundScheduleImageShareOutcome {
    SharedWithSystemSheet,
    DownloadedPngFallback,
}

pub fn build_round_schedule_share_snapshot(
    session_label: String,
    round_number: u32,
    players_by_id: &HashMap<PlayerId, Player>,
    scheduled_matches: &[&ScheduledMatch],
) -> RoundScheduleShareSnapshot {
    let mut sorted_matches: Vec<&ScheduledMatch> = scheduled_matches.to_vec();
    sorted_matches.sort_by_key(|scheduled_match| scheduled_match.field);

    let mut scheduled_player_ids = HashSet::new();
    let scheduled_field_cards = sorted_matches
        .into_iter()
        .map(|scheduled_match| {
            for player_id in scheduled_match
                .team_a
                .iter()
                .chain(scheduled_match.team_b.iter())
            {
                scheduled_player_ids.insert(*player_id);
            }

            RoundScheduleFieldShareCard {
                field_label: format!("Field {}", scheduled_match.field),
                team_a_player_names: scheduled_match
                    .team_a
                    .iter()
                    .map(|player_id| lookup_player_name(players_by_id, *player_id))
                    .collect(),
                team_b_player_names: scheduled_match
                    .team_b
                    .iter()
                    .map(|player_id| lookup_player_name(players_by_id, *player_id))
                    .collect(),
            }
        })
        .collect();

    let mut benched_player_names: Vec<String> = players_by_id
        .values()
        .filter(|player| player.status == PlayerStatus::Active)
        .filter(|player| !scheduled_player_ids.contains(&player.id))
        .map(|player| player.name.clone())
        .collect();
    benched_player_names.sort_by_cached_key(|player_name| player_name.to_ascii_lowercase());

    RoundScheduleShareSnapshot {
        session_label,
        round_number,
        scheduled_field_cards,
        benched_player_names,
    }
}

pub fn format_round_schedule_share_text(snapshot: &RoundScheduleShareSnapshot) -> String {
    let mut lines = vec![
        snapshot.session_label.clone(),
        format!("Round {}", snapshot.round_number),
        String::new(),
    ];

    for field_card in &snapshot.scheduled_field_cards {
        lines.push(field_card.field_label.clone());
        lines.push(field_card.team_a_player_names.join(" / "));
        lines.push("vs".to_string());
        lines.push(field_card.team_b_player_names.join(" / "));
        lines.push(String::new());
    }

    if !snapshot.benched_player_names.is_empty() {
        lines.push("Bench".to_string());
        lines.push(snapshot.benched_player_names.join(", "));
    } else {
        while lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }
    }

    lines.join("\n")
}

pub async fn copy_text_to_clipboard(content: &str) -> Result<(), String> {
    let Some(window) = web_sys::window() else {
        return Err("Window not available.".to_string());
    };

    let promise = window.navigator().clipboard().write_text(content);
    JsFuture::from(promise)
        .await
        .map(|_| ())
        .map_err(js_error_to_message)
}

pub async fn share_round_schedule_image(
    snapshot: &RoundScheduleShareSnapshot,
) -> Result<RoundScheduleImageShareOutcome, String> {
    let filename = round_schedule_png_filename(snapshot.round_number);
    let png_data_url = render_round_schedule_png_data_url(snapshot)?;

    if !browser_can_attempt_file_share() {
        trigger_data_url_download(&png_data_url, &filename)?;
        return Ok(RoundScheduleImageShareOutcome::DownloadedPngFallback);
    }

    let png_file = convert_data_url_into_file(&png_data_url, &filename).await?;
    if !browser_supports_file_share(&png_file) {
        trigger_data_url_download(&png_data_url, &filename)?;
        return Ok(RoundScheduleImageShareOutcome::DownloadedPngFallback);
    }

    let Some(window) = web_sys::window() else {
        return Err("Window not available.".to_string());
    };
    let navigator = window.navigator();

    let share_data = Object::new();
    Reflect::set(
        &share_data,
        &JsValue::from_str("title"),
        &JsValue::from_str(&format!("Round {}", snapshot.round_number)),
    )
    .map_err(js_error_to_message)?;
    Reflect::set(
        &share_data,
        &JsValue::from_str("text"),
        &JsValue::from_str(&format!(
            "{} schedule for Round {}",
            snapshot.session_label, snapshot.round_number
        )),
    )
    .map_err(js_error_to_message)?;

    let files = Array::new();
    files.push(&png_file);
    Reflect::set(&share_data, &JsValue::from_str("files"), &files.into())
        .map_err(js_error_to_message)?;

    let share_fn = Reflect::get(navigator.as_ref(), &JsValue::from_str("share"))
        .map_err(js_error_to_message)?;
    let share_fn: Function = share_fn
        .dyn_into()
        .map_err(|_| "Native share is not available here.".to_string())?;
    let share_promise_value = share_fn
        .call1(navigator.as_ref(), &share_data.into())
        .map_err(js_error_to_message)?;
    let share_promise: Promise = share_promise_value
        .dyn_into()
        .map_err(|_| "Share did not return a promise.".to_string())?;

    JsFuture::from(share_promise)
        .await
        .map_err(js_error_to_message)?;

    Ok(RoundScheduleImageShareOutcome::SharedWithSystemSheet)
}

fn lookup_player_name(players_by_id: &HashMap<PlayerId, Player>, player_id: PlayerId) -> String {
    players_by_id
        .get(&player_id)
        .map(|player| player.name.clone())
        .unwrap_or_else(|| format!("Player {}", player_id.0))
}

fn round_schedule_png_filename(round_number: u32) -> String {
    format!("pcplayerpicker-round-{round_number}.png")
}

fn browser_can_attempt_file_share() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    let navigator = window.navigator();
    Reflect::get(navigator.as_ref(), &JsValue::from_str("share"))
        .ok()
        .is_some_and(|share_fn| share_fn.is_function())
}

fn browser_supports_file_share(png_file: &web_sys::File) -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    let navigator = window.navigator();
    let Ok(can_share_fn) = Reflect::get(navigator.as_ref(), &JsValue::from_str("canShare")) else {
        return false;
    };
    let Ok(can_share_fn) = can_share_fn.dyn_into::<Function>() else {
        return false;
    };

    let share_data = Object::new();
    let files = Array::new();
    files.push(png_file);
    if Reflect::set(&share_data, &JsValue::from_str("files"), &files.into()).is_err() {
        return false;
    }

    can_share_fn
        .call1(navigator.as_ref(), &share_data.into())
        .ok()
        .and_then(|result| result.as_bool())
        .unwrap_or(false)
}

async fn convert_data_url_into_file(
    png_data_url: &str,
    filename: &str,
) -> Result<web_sys::File, String> {
    let Some(window) = web_sys::window() else {
        return Err("Window not available.".to_string());
    };

    let response_value = JsFuture::from(window.fetch_with_str(png_data_url))
        .await
        .map_err(js_error_to_message)?;
    let response: web_sys::Response = response_value
        .dyn_into()
        .map_err(|_| "Could not decode the generated image response.".to_string())?;
    let blob_promise = response.blob().map_err(js_error_to_message)?;
    let blob_value = JsFuture::from(blob_promise)
        .await
        .map_err(js_error_to_message)?;
    let blob: web_sys::Blob = blob_value
        .dyn_into()
        .map_err(|_| "Could not read the generated PNG blob.".to_string())?;

    let file_parts = Array::new();
    file_parts.push(&blob);
    web_sys::File::new_with_blob_sequence(&file_parts, filename).map_err(js_error_to_message)
}

fn render_round_schedule_png_data_url(
    snapshot: &RoundScheduleShareSnapshot,
) -> Result<String, String> {
    let Some(window) = web_sys::window() else {
        return Err("Window not available.".to_string());
    };
    let Some(document) = window.document() else {
        return Err("Document not available.".to_string());
    };

    let canvas: web_sys::HtmlCanvasElement = document
        .create_element("canvas")
        .map_err(js_error_to_message)?
        .dyn_into()
        .map_err(|_| "Could not create a canvas for the schedule image.".to_string())?;
    let context_value = canvas
        .get_context("2d")
        .map_err(js_error_to_message)?
        .ok_or_else(|| "2D canvas context unavailable.".to_string())?;
    let context: web_sys::CanvasRenderingContext2d = context_value
        .dyn_into()
        .map_err(|_| "Could not access the 2D drawing context.".to_string())?;

    let render_cards = build_render_cards(snapshot);
    let canvas_width = 1080u32;
    let header_height = 196u32;
    let outer_padding = 54u32;
    let card_gap = 24u32;
    let card_header_height = 30u32;
    let card_body_line_height = 36u32;
    let card_vertical_padding = 28u32;

    let total_card_height: u32 = render_cards
        .iter()
        .map(|render_card| {
            card_vertical_padding * 2
                + card_header_height
                + 12
                + (render_card.body_lines.len() as u32 * card_body_line_height)
        })
        .sum();
    let card_gap_total = card_gap.saturating_mul(render_cards.len().saturating_sub(1) as u32);
    let canvas_height =
        header_height + outer_padding + total_card_height + card_gap_total + outer_padding;

    canvas.set_width(canvas_width);
    canvas.set_height(canvas_height);

    context.set_fill_style_str("#030712");
    context.fill_rect(0.0, 0.0, canvas_width as f64, canvas_height as f64);

    context.set_fill_style_str("#0f766e");
    context.fill_rect(0.0, 0.0, canvas_width as f64, 18.0);

    context.set_fill_style_str("#5eead4");
    context.fill_rect(0.0, 18.0, canvas_width as f64, 6.0);

    context.set_fill_style_str("#f8fafc");
    context.set_font("700 50px ui-sans-serif, system-ui, sans-serif");
    context.set_text_baseline("top");
    let _ = context.fill_text(&format!("Round {}", snapshot.round_number), 54.0, 48.0);

    context.set_fill_style_str("#94a3b8");
    context.set_font("500 26px ui-sans-serif, system-ui, sans-serif");
    let _ = context.fill_text(&snapshot.session_label, 54.0, 110.0);

    context.set_fill_style_str("#cbd5e1");
    context.set_font("500 24px ui-sans-serif, system-ui, sans-serif");
    let schedule_summary = match snapshot.benched_player_names.len() {
        0 => format!("{} matches scheduled", snapshot.scheduled_field_cards.len()),
        bench_count => format!(
            "{} matches scheduled • {} on bench",
            snapshot.scheduled_field_cards.len(),
            bench_count
        ),
    };
    let _ = context.fill_text(&schedule_summary, 54.0, 146.0);

    let mut card_top = header_height as f64;
    for render_card in render_cards {
        let card_height = (card_vertical_padding * 2
            + card_header_height
            + 12
            + (render_card.body_lines.len() as u32 * card_body_line_height))
            as f64;
        let card_left = outer_padding as f64;
        let card_width = (canvas_width - (outer_padding * 2)) as f64;

        context.set_fill_style_str("#111827");
        context.fill_rect(card_left, card_top, card_width, card_height);

        context.set_stroke_style_str("#1f2937");
        context.set_line_width(2.0);
        context.stroke_rect(card_left, card_top, card_width, card_height);

        context.set_fill_style_str("#5eead4");
        context.fill_rect(card_left, card_top, 10.0, card_height);

        context.set_fill_style_str("#e2e8f0");
        context.set_font("700 26px ui-sans-serif, system-ui, sans-serif");
        let _ = context.fill_text(
            &render_card.card_title,
            card_left + 28.0,
            card_top + card_vertical_padding as f64,
        );

        context.set_fill_style_str("#f8fafc");
        context.set_font("600 28px ui-sans-serif, system-ui, sans-serif");
        let mut line_top =
            card_top + card_vertical_padding as f64 + card_header_height as f64 + 12.0;
        for line in render_card.body_lines {
            let color = if line == "vs" { "#5eead4" } else { "#f8fafc" };
            context.set_fill_style_str(color);
            let _ = context.fill_text(&line, card_left + 28.0, line_top);
            line_top += card_body_line_height as f64;
        }

        card_top += card_height + card_gap as f64;
    }

    canvas
        .to_data_url_with_type("image/png")
        .map_err(js_error_to_message)
}

fn build_render_cards(snapshot: &RoundScheduleShareSnapshot) -> Vec<RoundScheduleRenderCard> {
    let mut render_cards: Vec<RoundScheduleRenderCard> = snapshot
        .scheduled_field_cards
        .iter()
        .map(|field_card| {
            let mut body_lines =
                wrap_text_to_line_width(&field_card.team_a_player_names.join(" / "), 34);
            body_lines.push("vs".to_string());
            body_lines.extend(wrap_text_to_line_width(
                &field_card.team_b_player_names.join(" / "),
                34,
            ));

            RoundScheduleRenderCard {
                card_title: field_card.field_label.clone(),
                body_lines,
            }
        })
        .collect();

    if !snapshot.benched_player_names.is_empty() {
        render_cards.push(RoundScheduleRenderCard {
            card_title: "Bench".to_string(),
            body_lines: wrap_text_to_line_width(&snapshot.benched_player_names.join(", "), 42),
        });
    }

    render_cards
}

fn wrap_text_to_line_width(text: &str, max_characters_per_line: usize) -> Vec<String> {
    let mut wrapped_lines = Vec::new();
    let mut current_line = String::new();

    for word in text.split_whitespace() {
        let candidate_line = if current_line.is_empty() {
            word.to_string()
        } else {
            format!("{current_line} {word}")
        };

        if candidate_line.chars().count() <= max_characters_per_line {
            current_line = candidate_line;
        } else {
            if !current_line.is_empty() {
                wrapped_lines.push(current_line);
            }
            current_line = word.to_string();
        }
    }

    if !current_line.is_empty() {
        wrapped_lines.push(current_line);
    }

    if wrapped_lines.is_empty() {
        wrapped_lines.push(String::new());
    }

    wrapped_lines
}

fn trigger_data_url_download(data_url: &str, filename: &str) -> Result<(), String> {
    let Some(window) = web_sys::window() else {
        return Err("Window not available.".to_string());
    };
    let Some(document) = window.document() else {
        return Err("Document not available.".to_string());
    };

    let anchor: web_sys::HtmlAnchorElement = document
        .create_element("a")
        .map_err(js_error_to_message)?
        .dyn_into()
        .map_err(|_| "Could not create a temporary download link.".to_string())?;
    anchor.set_href(data_url);
    anchor.set_download(filename);
    anchor
        .set_attribute("style", "display:none")
        .map_err(js_error_to_message)?;

    let Some(body) = document.body() else {
        return Err("Document body not available.".to_string());
    };
    let _ = body.append_child(&anchor).map_err(js_error_to_message)?;
    anchor.click();
    let _ = body.remove_child(&anchor).map_err(js_error_to_message)?;
    Ok(())
}

fn js_error_to_message(error: JsValue) -> String {
    error
        .dyn_ref::<js_sys::Error>()
        .map(|js_error| js_error.message().into())
        .or_else(|| error.as_string())
        .unwrap_or_else(|| "Unexpected browser error.".to_string())
}

struct RoundScheduleRenderCard {
    card_title: String,
    body_lines: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        build_render_cards, build_round_schedule_share_snapshot, format_round_schedule_share_text,
        wrap_text_to_line_width, RoundScheduleFieldShareCard, RoundScheduleShareSnapshot,
    };
    use app_core::models::{
        MatchId, MatchStatus, Player, PlayerId, PlayerStatus, RoundNumber, ScheduledMatch,
    };
    use std::collections::HashMap;

    #[test]
    fn share_snapshot_collects_bench_players_not_in_matches() {
        let players_by_id: HashMap<PlayerId, Player> = [
            make_player(1, "Alice"),
            make_player(2, "Brooke"),
            make_player(3, "Cara"),
            make_player(4, "Dana"),
            make_player(5, "Eden"),
        ]
        .into_iter()
        .map(|player| (player.id, player))
        .collect();
        let round_matches = [make_match(1, 1, vec![1, 2], vec![3, 4])];

        let snapshot = build_round_schedule_share_snapshot(
            "Soccer 2v2".to_string(),
            1,
            &players_by_id,
            &round_matches.iter().collect::<Vec<_>>(),
        );

        assert_eq!(snapshot.scheduled_field_cards.len(), 1);
        assert_eq!(snapshot.benched_player_names, vec!["Eden".to_string()]);
    }

    #[test]
    fn share_text_uses_group_chat_friendly_layout() {
        let players_by_id: HashMap<PlayerId, Player> = [
            make_player(1, "Alice"),
            make_player(2, "Brooke"),
            make_player(3, "Cara"),
            make_player(4, "Dana"),
        ]
        .into_iter()
        .map(|player| (player.id, player))
        .collect();
        let round_matches = [make_match(2, 3, vec![1, 2], vec![3, 4])];

        let snapshot = build_round_schedule_share_snapshot(
            "Soccer 2v2".to_string(),
            3,
            &players_by_id,
            &round_matches.iter().collect::<Vec<_>>(),
        );
        let share_text = format_round_schedule_share_text(&snapshot);

        assert_eq!(
            share_text,
            "Soccer 2v2\nRound 3\n\nField 2\nAlice / Brooke\nvs\nCara / Dana"
        );
    }

    #[test]
    fn share_snapshot_sorts_fields_and_skips_inactive_players_from_bench() {
        let players_by_id: HashMap<PlayerId, Player> = [
            make_player(1, "Alice"),
            make_player(2, "Brooke"),
            make_player(3, "Cara"),
            make_player(4, "Dana"),
            make_player(5, "Eden"),
            make_inactive_player(6, "Former Player"),
        ]
        .into_iter()
        .map(|player| (player.id, player))
        .collect();
        let round_matches = [
            make_match(3, 7, vec![1, 2], vec![3, 4]),
            make_match(1, 8, vec![2, 3], vec![4, 5]),
        ];

        let snapshot = build_round_schedule_share_snapshot(
            "Soccer 2v2".to_string(),
            2,
            &players_by_id,
            &round_matches.iter().collect::<Vec<_>>(),
        );

        let ordered_field_labels: Vec<_> = snapshot
            .scheduled_field_cards
            .iter()
            .map(|field_card| field_card.field_label.as_str())
            .collect();
        assert_eq!(ordered_field_labels, vec!["Field 1", "Field 3"]);
        assert!(snapshot.benched_player_names.is_empty());
    }

    #[test]
    fn render_cards_append_a_bench_card_when_someone_sits_out() {
        let render_cards = build_render_cards(&RoundScheduleShareSnapshot {
            session_label: "Soccer 2v2".to_string(),
            round_number: 4,
            scheduled_field_cards: vec![RoundScheduleFieldShareCard {
                field_label: "Field 1".to_string(),
                team_a_player_names: vec!["Alice".to_string(), "Brooke".to_string()],
                team_b_player_names: vec!["Cara".to_string(), "Dana".to_string()],
            }],
            benched_player_names: vec!["Eden".to_string(), "Fiona".to_string()],
        });

        assert_eq!(render_cards.len(), 2);
        assert_eq!(render_cards[1].card_title, "Bench");
        assert_eq!(render_cards[1].body_lines, vec!["Eden, Fiona".to_string()]);
    }

    #[test]
    fn wrap_text_keeps_lines_within_requested_width() {
        let wrapped = wrap_text_to_line_width("Alice / Brooke / Catherine / Daniela", 18);
        assert_eq!(
            wrapped,
            vec![
                "Alice / Brooke /".to_string(),
                "Catherine /".to_string(),
                "Daniela".to_string()
            ]
        );
    }

    fn make_player(player_number: u32, player_name: &str) -> Player {
        Player {
            id: PlayerId(player_number),
            name: player_name.to_string(),
            status: PlayerStatus::Active,
            joined_at_round: RoundNumber(1),
            deactivated_at_round: None,
        }
    }

    fn make_inactive_player(player_number: u32, player_name: &str) -> Player {
        Player {
            id: PlayerId(player_number),
            name: player_name.to_string(),
            status: PlayerStatus::Inactive,
            joined_at_round: RoundNumber(1),
            deactivated_at_round: Some(RoundNumber(2)),
        }
    }

    fn make_match(
        field_number: u8,
        match_number: u32,
        team_a_player_numbers: Vec<u32>,
        team_b_player_numbers: Vec<u32>,
    ) -> ScheduledMatch {
        ScheduledMatch {
            id: MatchId(match_number),
            round: RoundNumber(1),
            field: field_number,
            team_a: team_a_player_numbers.into_iter().map(PlayerId).collect(),
            team_b: team_b_player_numbers.into_iter().map(PlayerId).collect(),
            status: MatchStatus::Scheduled,
        }
    }
}
