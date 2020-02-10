use ed25519_dalek::PublicKey;
use failure::Error;
use failure::ResultExt;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::params;
use rusqlite::OptionalExtension;

use crate::message::{NewInboundMessage, NewOutboundMessage};

pub type DbPool = Pool<SqliteConnectionManager>;
lazy_static::lazy_static! {
    static ref POOL: DbPool = Pool::new(SqliteConnectionManager::file("messages.db")).expect("POOL");
}

pub async fn migrate() -> Result<(), Error> {
    let pool = POOL.clone();
    tokio::task::spawn_blocking(move || {
        let mut gconn = pool.get()?;
        let conn = gconn.transaction()?;
        let q =
            "SELECT count(name) FROM sqlite_master WHERE type = 'table' AND name = 'migrations'";
        let exists: i64 = conn
            .query_row(q, params![], |row| row.get(0))
            .with_context(|e| format!("{}: {}", q, e))?;
        let q = "SELECT * FROM migrations WHERE name = 'init'";
        if exists == 0
            || conn
                .query_row(q, params![], |_| Ok(()))
                .optional()
                .with_context(|e| format!("{}: {}", q, e))?
                .is_none()
        {
            let q = "CREATE TABLE messages (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        user_id BLOB NOT NULL,
                        created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                        inbound BOOLEAN NOT NULL,
                        time INTEGER NOT NULL,
                        content TEXT NOT NULL,
                        read BOOLEAN NOT NULL DEFAULT FALSE
                    )";
            conn.execute(q, params![])
                .with_context(|e| format!("{}: {}", q, e))?;
            let q = "CREATE TABLE users (
                        id BLOB PRIMARY KEY,
                        name TEXT NOT NULL
                    )";
            conn.execute(q, params![])
                .with_context(|e| format!("{}: {}", q, e))?;
            let q = "CREATE TABLE migrations (
                        time TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                        name TEXT
                    )";
            conn.execute(q, params![])
                .with_context(|e| format!("{}: {}", q, e))?;
            let q = "INSERT INTO migrations (name) VALUES ('init')";
            conn.execute(q, params![])
                .with_context(|e| format!("{}: {}", q, e))?;
            conn.commit()?;
        }
        Ok::<_, Error>(())
    })
    .await??;
    Ok(())
}

pub async fn get_message_count_by_user(pubkey: PublicKey) -> Result<i64, Error> {
    let pool = POOL.clone();
    let res = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let res = conn.query_row(
            "SELECT count(id) FROM messages WHERE user_id = ?1",
            params![&pubkey.as_bytes()[..]],
            |row| row.get(0),
        )?;
        Ok::<_, Error>(res)
    })
    .await??;
    Ok(res)
}

pub async fn save_in_message(message: NewInboundMessage) -> Result<(), Error> {
    let pool = POOL.clone();
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        conn.execute(
            "INSERT INTO messages (user_id, inbound, time, content) VALUES (?1, true, ?2, ?3)",
            params![&message.from.as_bytes()[..], message.time, message.content],
        )?;
        Ok::<_, Error>(())
    })
    .await??;
    Ok(())
}

pub async fn save_out_message(message: NewOutboundMessage) -> Result<(), Error> {
    let pool = POOL.clone();
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        conn.execute(
            "INSERT INTO messages (user_id, inbound, time, content, read) VALUES (?1, false, ?2, ?3, true)",
            params![&message.to.as_bytes()[..], message.time, message.content],
        )?;
        Ok::<_, Error>(())
    })
    .await??;
    Ok(())
}

pub async fn save_user(pubkey: PublicKey, name: String) -> Result<(), Error> {
    let pool = POOL.clone();
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        conn.execute(
            "INSERT INTO users (id, name) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET name = excluded.name",
            params![&pubkey.as_bytes()[..], name],
        )?;
        Ok::<_, Error>(())
    })
    .await??;
    Ok(())
}

pub async fn get_user(pubkey: PublicKey) -> Result<Option<String>, Error> {
    let pool = POOL.clone();
    let res = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let res = conn
            .query_row(
                "SELECT name FROM users WHERE id = ?1",
                params![&pubkey.as_bytes()[..]],
                |row| row.get(0),
            )
            .optional()?;
        Ok::<_, Error>(res)
    })
    .await??;
    Ok(res)
}

pub async fn del_user(pubkey: PublicKey) -> Result<(), Error> {
    let pool = POOL.clone();
    let res = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        conn.execute(
            "DELETE FROM users WHERE id = ?1",
            params![&pubkey.as_bytes()[..]],
        )?;
        Ok::<_, Error>(())
    })
    .await??;
    Ok(res)
}

#[derive(Clone, Debug)]
pub struct UserInfo {
    pub pubkey: PublicKey,
    pub name: Option<String>,
    pub unreads: i64,
}

pub async fn get_user_info() -> Result<Vec<UserInfo>, Error> {
    let pool = POOL.clone();
    let res = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let mut stmt = conn.prepare(
            "SELECT
                messages.user_id,
                users.name,
                SUM(CASE WHEN messages.read THEN 0 ELSE 1 END)
            FROM messages
            LEFT JOIN users
            ON messages.user_id = users.id
            GROUP BY users.id, users.name
            UNION ALL
            SELECT
                users.id,
                users.name,
                count(messages.id)
            FROM users
            LEFT JOIN messages
            ON messages.user_id = users.id
            WHERE messages.user_id IS NULL
            GROUP BY users.id, users.name",
        )?;
        let res = stmt
            .query_map(params![], |row| {
                let uid: Vec<u8> = row.get(0)?;
                Ok(UserInfo {
                    pubkey: PublicKey::from_bytes(&uid).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Blob,
                            Box::new(e),
                        )
                    })?,
                    name: row.get(1)?,
                    unreads: row.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok::<_, Error>(res)
    })
    .await??;
    Ok(res)
}

#[derive(Clone, Debug)]
pub struct Message {
    pub time: i64,
    pub inbound: bool,
    pub content: String,
}
pub async fn get_messages(
    pubkey: PublicKey,
    limit: Option<usize>,
    mark_as_read: bool,
) -> Result<Vec<Message>, Error> {
    let pool = POOL.clone();
    let res = tokio::task::spawn_blocking(move || {
        let mut gconn = pool.get()?;
        let conn = gconn.transaction()?;
        if mark_as_read {
            if let Some(limit) = limit {
                conn.execute(&format!("UPDATE messages SET read = true WHERE user_id = ?1 AND id IN (SELECT id FROM messages WHERE user_id = ?1 ORDER BY created_at DESC LIMIT {})", limit), params![&pubkey.as_bytes()[..]])?;
            } else {
                conn.execute(&format!("UPDATE messages SET read = true WHERE user_id = ?1"), params![&pubkey.as_bytes()[..]])?;
            }
        }
        let mut stmt =
            if let Some(limit) = limit {
                conn.prepare(&format!("SELECT time, inbound, content FROM messages WHERE user_id = ?1 ORDER BY created_at DESC LIMIT {}", limit))?
            } else {
                conn.prepare("SELECT time, inbound, content FROM messages WHERE user_id = ?1 ORDER BY created_at DESC")?
            };
        let res = stmt
            .query_map(params![&pubkey.as_bytes()[..]], |row| {
                Ok(Message {
                    time: row.get(0)?,
                    inbound: row.get(1)?,
                    content: row.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        conn.commit()?;
        Ok::<_, Error>(res)
    })
    .await??;
    Ok(res)
}
