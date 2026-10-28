use note_voice_proto::MicGain;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

/// Element X and other Matrix calls; their level is reliable, so they start at unity with auto off.
pub const ELEMENT: &str = "element";
pub const MAX_GAIN_DB: f32 = 24.0;
const MAX_LABEL: usize = 64;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MicRow {
    pub client: String,
    pub kind: &'static str,
    pub label: String,
    pub gain_db: f32,
    pub auto: bool,
}

/// What a client calls with: Element X, or a browser or desktop app's input device.
fn kind(client: &str) -> Option<&'static str> {
    if client == ELEMENT {
        return Some("element");
    }
    let (kind, key) = client.split_once(':')?;
    let key_ok = !key.is_empty() && key.len() <= 32 && key.bytes().all(|b| b.is_ascii_hexdigit());
    match kind {
        "web" if key_ok => Some("web"),
        "desktop" if key_ok => Some("desktop"),
        _ => None,
    }
}

/// The stored id of a browser or desktop app's device, when `client` and `key` are well formed.
pub fn device_client(client: &str, key: &str) -> Option<String> {
    let id = format!("{client}:{}", key.to_ascii_lowercase());
    kind(&id).filter(|k| *k != "element").map(|_| id)
}

fn default_gain(client: &str) -> MicGain {
    MicGain { gain_db: 0.0, auto: client != ELEMENT }
}

fn row(conn: &Connection, user_id: i64, client: &str) -> rusqlite::Result<Option<(String, MicGain)>> {
    conn.query_row(
        "SELECT label, gain_db, auto FROM mic_gains WHERE user_id = ?1 AND client = ?2",
        (user_id, client),
        |r| Ok((r.get(0)?, MicGain { gain_db: r.get::<_, f64>(1)? as f32, auto: r.get(2)? })),
    )
    .optional()
}

/// The gain a call on `client` starts from.
pub fn gain(conn: &Connection, user_id: i64, client: &str) -> MicGain {
    row(conn, user_id, client).ok().flatten().map_or_else(|| default_gain(client), |(_, g)| g)
}

/// Element X first, then each device by when it was last used or set.
pub fn list(conn: &Connection, user_id: i64) -> rusqlite::Result<Vec<MicRow>> {
    let mut stmt = conn.prepare(
        "SELECT client, label, gain_db, auto FROM mic_gains WHERE user_id = ?1 AND client != ?2
         ORDER BY updated_at DESC, client",
    )?;
    let devices = stmt
        .query_map((user_id, ELEMENT), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?, r.get::<_, bool>(3)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let element = gain(conn, user_id, ELEMENT);
    let mut rows =
        vec![MicRow { client: ELEMENT.into(), kind: "element", label: String::new(), gain_db: element.gain_db, auto: element.auto }];
    rows.extend(devices.into_iter().filter_map(|(client, label, gain_db, auto)| {
        Some(MicRow { kind: kind(&client)?, client, label, gain_db: gain_db as f32, auto })
    }));
    Ok(rows)
}

/// Notes a device a call came in on, keeping its gain; returns the gain to start from.
pub fn seen(conn: &Connection, user_id: i64, client: &str, label: &str, now: jiff::Timestamp) -> rusqlite::Result<MicGain> {
    let label: String = label.chars().map(|c| if c.is_control() { ' ' } else { c }).take(MAX_LABEL).collect();
    let label = label.trim();
    let start = default_gain(client);
    conn.execute(
        "INSERT INTO mic_gains (user_id, client, label, gain_db, auto, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (user_id, client) DO UPDATE SET label = excluded.label, updated_at = excluded.updated_at",
        (user_id, client, &label, f64::from(start.gain_db), start.auto, now.to_string()),
    )?;
    Ok(gain(conn, user_id, client))
}

#[derive(Debug, Default, Clone, Copy, serde::Deserialize)]
pub struct MicPatch {
    pub gain_db: Option<f32>,
    pub auto: Option<bool>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PatchError {
    UnknownClient,
    GainOutOfRange,
}

/// A gain set by hand turns auto off; auto turned on adapts from the gain there.
pub fn set(
    conn: &Connection,
    user_id: i64,
    client: &str,
    patch: MicPatch,
    now: jiff::Timestamp,
) -> Result<Result<MicRow, PatchError>, rusqlite::Error> {
    let Some(kind) = kind(client) else { return Ok(Err(PatchError::UnknownClient)) };
    let existing = row(conn, user_id, client)?;
    if existing.is_none() && client != ELEMENT {
        return Ok(Err(PatchError::UnknownClient));
    }
    let (label, mut mic) = existing.unwrap_or_else(|| (String::new(), default_gain(client)));
    if let Some(g) = patch.gain_db {
        if !(0.0..=MAX_GAIN_DB).contains(&g) {
            return Ok(Err(PatchError::GainOutOfRange));
        }
        mic = MicGain { gain_db: g, auto: false };
    }
    if let Some(auto) = patch.auto {
        mic.auto = auto;
    }
    conn.execute(
        "INSERT INTO mic_gains (user_id, client, label, gain_db, auto, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (user_id, client) DO UPDATE SET gain_db = excluded.gain_db, auto = excluded.auto,
             updated_at = excluded.updated_at",
        (user_id, client, &label, f64::from(mic.gain_db), mic.auto, now.to_string()),
    )?;
    Ok(Ok(MicRow { client: client.into(), kind, label, gain_db: mic.gain_db, auto: mic.auto }))
}

/// Keeps the gain auto reached on the call's client, unless the user has set it by hand since.
pub fn learned(conn: &Connection, call_id: &str, mic: MicGain) -> rusqlite::Result<()> {
    let call: Option<(i64, Option<String>)> = conn
        .query_row("SELECT user_id, mic_client FROM voice_calls WHERE id = ?1", [call_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((user_id, Some(client))) = call else { return Ok(()) };
    if !mic.auto || !mic.gain_db.is_finite() {
        return Ok(());
    }
    if !row(conn, user_id, &client)?.map_or_else(|| default_gain(&client), |(_, g)| g).auto {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO mic_gains (user_id, client, label, gain_db, auto, updated_at) VALUES (?1, ?2, '', ?3, 1, ?4)
         ON CONFLICT (user_id, client) DO UPDATE SET gain_db = excluded.gain_db",
        (user_id, &client, f64::from(mic.gain_db.clamp(0.0, MAX_GAIN_DB)), jiff::Timestamp::now().to_string()),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = crate::db::open_memory().unwrap();
        for (id, name) in [(1, "aitest"), (2, "other")] {
            conn.execute(
                "INSERT INTO users (id, username, pass_hash, role) VALUES (?1, ?2, 'x', 'member')",
                (id, name),
            )
            .unwrap();
        }
        conn
    }

    fn call(conn: &Connection, id: &str, user_id: i64, client: &str) {
        conn.execute(
            "INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at, mic_client)
             VALUES (?1, ?2, 'inbound', 'answered', 'x', 'x', ?3)",
            (id, user_id, client),
        )
        .unwrap();
    }

    fn now() -> jiff::Timestamp {
        jiff::Timestamp::now()
    }

    #[test]
    fn element_starts_at_unity_with_auto_off_and_devices_adapt() {
        let conn = db();
        assert_eq!(gain(&conn, 1, ELEMENT), MicGain { gain_db: 0.0, auto: false });
        let laptop = device_client("web", "AB12").unwrap();
        assert_eq!(laptop, "web:ab12");
        assert_eq!(seen(&conn, 1, &laptop, "Built-in\nMic", now()).unwrap(), MicGain { gain_db: 0.0, auto: true });
        let rows = list(&conn, 1).unwrap();
        assert_eq!(rows[0].client, ELEMENT);
        assert_eq!((rows[1].kind, rows[1].label.as_str()), ("web", "Built-in Mic"));
        call(&conn, "c1", 1, ELEMENT);
        learned(&conn, "c1", MicGain { gain_db: 9.0, auto: true }).unwrap();
        assert_eq!(gain(&conn, 1, ELEMENT), MicGain { gain_db: 0.0, auto: false }, "Element X stays put unless turned on");
        assert!(device_client("matrix", "ab").is_none() && device_client("web", "x y").is_none());
    }

    #[test]
    fn each_device_learns_its_own_gain() {
        let conn = db();
        let (laptop, headset, desk) = ("web:aa".to_string(), "web:bb".to_string(), "desktop:aa".to_string());
        for c in [&laptop, &headset, &desk] {
            seen(&conn, 1, c, "", now()).unwrap();
        }
        call(&conn, "c1", 1, &laptop);
        learned(&conn, "c1", MicGain { gain_db: 14.0, auto: true }).unwrap();
        assert_eq!(gain(&conn, 1, &laptop).gain_db, 14.0);
        assert_eq!(gain(&conn, 1, &headset).gain_db, 0.0);
        assert_eq!(gain(&conn, 1, &desk).gain_db, 0.0);
        assert_eq!(seen(&conn, 1, &laptop, "renamed", now()).unwrap().gain_db, 14.0, "seeing it again keeps the gain");
        assert_eq!(gain(&conn, 2, &laptop), MicGain { gain_db: 0.0, auto: true }, "another user's same device is apart");
    }

    #[test]
    fn a_manual_gain_wins_over_auto() {
        let conn = db();
        seen(&conn, 1, "web:aa", "", now()).unwrap();
        let set_row = set(&conn, 1, "web:aa", MicPatch { gain_db: Some(10.0), auto: None }, now()).unwrap().unwrap();
        assert_eq!((set_row.gain_db, set_row.auto), (10.0, false));
        call(&conn, "c1", 1, "web:aa");
        learned(&conn, "c1", MicGain { gain_db: 3.0, auto: true }).unwrap();
        assert_eq!(gain(&conn, 1, "web:aa"), MicGain { gain_db: 10.0, auto: false });
        let on = set(&conn, 1, ELEMENT, MicPatch { gain_db: None, auto: Some(true) }, now()).unwrap().unwrap();
        assert!(on.auto);
        call(&conn, "c2", 1, ELEMENT);
        learned(&conn, "c2", MicGain { gain_db: 5.0, auto: true }).unwrap();
        assert_eq!(gain(&conn, 1, ELEMENT), MicGain { gain_db: 5.0, auto: true });
    }

    #[test]
    fn a_user_cannot_reach_another_users_devices() {
        let conn = db();
        seen(&conn, 2, "web:aa", "theirs", now()).unwrap();
        assert_eq!(list(&conn, 1).unwrap().len(), 1, "only Element X");
        let patch = MicPatch { gain_db: Some(20.0), auto: None };
        assert_eq!(set(&conn, 1, "web:aa", patch, now()).unwrap(), Err(PatchError::UnknownClient));
        assert_eq!(gain(&conn, 2, "web:aa").gain_db, 0.0);
        assert_eq!(set(&conn, 1, ELEMENT, MicPatch { gain_db: Some(25.0), auto: None }, now()).unwrap(), Err(PatchError::GainOutOfRange));
        assert_eq!(set(&conn, 1, "nope", MicPatch::default(), now()).unwrap(), Err(PatchError::UnknownClient));
    }
}
