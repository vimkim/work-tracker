use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::Html,
    routing::get,
};
use chrono::Local;

use crate::{db::Tracker, domain::WorkItem};

#[derive(Clone)]
struct AppState {
    database: PathBuf,
}

type WebResult = Result<Html<String>, (StatusCode, Html<String>)>;

pub async fn serve(database: PathBuf, bind: &str) -> Result<()> {
    let address: SocketAddr = bind
        .parse()
        .with_context(|| format!("invalid bind address: {bind}"))?;
    let app = router(database);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind {address}"))?;
    println!("Work Tracker dashboard: http://{address}");
    axum::serve(listener, app).await?;
    Ok(())
}

fn router(database: PathBuf) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/items/{id}", get(show_item))
        .with_state(AppState { database })
}

async fn index(State(state): State<AppState>) -> WebResult {
    let tracker = Tracker::open(&state.database).map_err(internal_error)?;
    let items = tracker.daily_view(false).map_err(internal_error)?;
    let cards = if items.is_empty() {
        "<p class=\"empty\">No work items in the daily view.</p>".to_owned()
    } else {
        items.iter().map(item_card).collect::<Vec<_>>().join("\n")
    };
    Ok(Html(page(
        "Daily view",
        &format!(
            "<header><div><p class=\"eyebrow\">WORK TRACKER</p><h1>Daily view</h1><p>Updated today and all actionable work.</p></div><span class=\"date\">{}</span></header><main class=\"grid\">{cards}</main>",
            Local::now().format("%A, %B %-d")
        ),
    )))
}

async fn show_item(State(state): State<AppState>, Path(id): Path<i64>) -> WebResult {
    let tracker = Tracker::open(&state.database).map_err(internal_error)?;
    let item = match tracker.get(id) {
        Ok(item) => item,
        Err(_) => {
            return Err((
                StatusCode::NOT_FOUND,
                Html(page(
                    "Not found",
                    "<main><a href=\"/\">← Daily view</a><h1>Work item not found</h1></main>",
                )),
            ));
        }
    };
    let history = tracker.history(id).map_err(internal_error)?;
    let description = item
        .description
        .as_deref()
        .map(|value| format!("<p class=\"description\">{}</p>", escape(value)))
        .unwrap_or_default();
    let history_html = history
        .iter()
        .rev()
        .map(|entry| {
            let note = entry
                .note
                .as_deref()
                .map(|note| format!("<p>{}</p>", escape(note)))
                .unwrap_or_default();
            format!(
                "<li><div><strong>{}</strong><span>by {}</span></div><time>{}</time>{note}</li>",
                escape(&entry.kind.replace('_', " ")),
                escape(&entry.actor),
                entry
                    .occurred_at
                    .with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M:%S")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let body = format!(
        "<main><a class=\"back\" href=\"/\">← Daily view</a><article class=\"detail\"><div class=\"meta\"><span class=\"status status-{}\">{}</span><span>#{} · updated {}</span></div><h1>{}</h1>{description}</article><section class=\"history\"><h2>History</h2><ol>{history_html}</ol></section></main>",
        item.status,
        item.status,
        item.id,
        item.updated_at
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M"),
        escape(&item.title),
    );
    Ok(Html(page(&item.title, &body)))
}

fn item_card(item: &WorkItem) -> String {
    let description = item
        .description
        .as_deref()
        .map(|value| format!("<p>{}</p>", escape(value)))
        .unwrap_or_default();
    format!(
        "<a class=\"card\" href=\"/items/{}\"><div class=\"meta\"><span class=\"status status-{}\">{}</span><span>#{}</span></div><h2>{}</h2>{description}<time>Updated {}</time></a>",
        item.id,
        item.status,
        item.status,
        item.id,
        escape(&item.title),
        item.updated_at
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
    )
}

fn internal_error(error: anyhow::Error) -> (StatusCode, Html<String>) {
    eprintln!("dashboard error: {error:#}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Html(page(
            "Server error",
            "<main><h1>Unable to read the tracker database</h1></main>",
        )),
    )
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{} · Work Tracker</title><style>{}</style></head><body>{body}</body></html>",
        escape(title),
        CSS
    )
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

const CSS: &str = r#"
:root{color-scheme:dark;--bg:#10130f;--panel:#191e18;--line:#30382e;--text:#edf3e9;--muted:#9da99a;--accent:#b7f071}*{box-sizing:border-box}body{margin:0;background:radial-gradient(circle at 15% 0,#243020 0,transparent 32rem),var(--bg);color:var(--text);font:16px/1.5 ui-sans-serif,system-ui,sans-serif}header,main{width:min(1100px,calc(100% - 2rem));margin:auto}header{display:flex;justify-content:space-between;align-items:end;padding:4rem 0 2rem;border-bottom:1px solid var(--line)}h1,h2,p{margin-top:0}h1{font-size:clamp(2.2rem,6vw,4.5rem);line-height:1;letter-spacing:-.04em;margin-bottom:.8rem}header p{color:var(--muted)}.eyebrow{color:var(--accent);font-size:.75rem;letter-spacing:.18em;font-weight:800}.date{color:var(--muted);padding-bottom:1rem}.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(280px,1fr));gap:1rem;padding:2rem 0 5rem}.card,.detail,.history{background:color-mix(in srgb,var(--panel) 94%,transparent);border:1px solid var(--line);border-radius:14px}.card{display:block;min-height:220px;padding:1.35rem;color:inherit;text-decoration:none;transition:.15s transform,.15s border-color}.card:hover{transform:translateY(-3px);border-color:var(--accent)}.card h2{margin:2rem 0 .7rem;line-height:1.2}.card p,.description{color:#c5cec1}.card time{display:block;color:var(--muted);font-size:.8rem;margin-top:1.5rem}.meta{display:flex;align-items:center;gap:.7rem;color:var(--muted);font-size:.8rem}.status{display:inline-block;padding:.22rem .55rem;border-radius:999px;background:#30382e;color:#dce7d8;font-weight:750;text-transform:uppercase;font-size:.66rem;letter-spacing:.06em}.status-active{background:#325f2c;color:#d8ffd0}.status-blocked{background:#6b302d;color:#ffd7d4}.status-waiting{background:#5b4c24;color:#fff0b5}.status-done{background:#234d55;color:#c9f5ff}.status-deleted,.status-cancelled{background:#3a3a3a;color:#ccc}.back{display:inline-block;margin:2rem 0;color:var(--accent);text-decoration:none}.detail{padding:clamp(1.5rem,5vw,4rem)}.detail h1{margin:1.5rem 0;font-size:clamp(2rem,5vw,4rem)}.history{margin:1rem 0 5rem;padding:1.5rem}.history ol{list-style:none;padding:0;margin:0}.history li{border-top:1px solid var(--line);padding:1rem 0}.history li div{display:flex;gap:.5rem}.history li span,.history time{color:var(--muted);font-size:.85rem}.history li time{display:block}.history li p{margin:.55rem 0 0}.empty{padding:3rem 0;color:var(--muted)}@media(max-width:600px){header{align-items:start;flex-direction:column}.date{padding:0}}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_escape_handles_markup_and_quotes() {
        assert_eq!(
            escape("<script a='b'>&\"</script>"),
            "&lt;script a=&#39;b&#39;&gt;&amp;&quot;&lt;/script&gt;"
        );
    }
}
