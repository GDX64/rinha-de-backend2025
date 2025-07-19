use std::fmt::Display;

use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct DBPlayer {
    name: String,
    kills: usize,
    deaths: usize,
}

pub struct PaymentsDb {
    conn: rusqlite::Connection,
}

pub enum PaymentKind {
    Default,
    Fallback,
}

impl Display for PaymentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PaymentKind::Default => write!(f, "default"),
            PaymentKind::Fallback => write!(f, "fallback"),
        }
    }
}

impl PaymentsDb {
    pub fn new() -> anyhow::Result<Self> {
        let conn = rusqlite::Connection::open_in_memory()?;

        conn.execute(
            "create table if not exists payments (
                  uuid text primary key,
                  amount int not null,
                  requested_at timestamp not null,
                  kind text not null
             )",
            rusqlite::params![],
        )?;
        Ok(Self { conn })
    }

    pub fn insert_payment(&self, post: &PaymentPost, kind: PaymentKind) -> anyhow::Result<()> {
        self.conn.execute(
            "insert into payments (uuid, amount, requested_at, kind) values (?1, ?2, ?3, ?4)",
            rusqlite::params![
                post.correlation_id,
                post.cents(),
                post.requested_at_ts,
                kind.to_string()
            ],
        )?;
        Ok(())
    }

    fn parse_date(date_str: &str) -> anyhow::Result<i64> {
        chrono::DateTime::parse_from_rfc3339(date_str)
            .map(|dt| dt.timestamp_millis())
            .map_err(|e| anyhow::anyhow!("Invalid date format: {}", e))
    }

    fn date_from_millis(millis: i64) -> String {
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    pub fn get_stats(&self, from: Option<&str>, to: Option<&str>) -> anyhow::Result<Stats> {
        let from = if let Some(from) = from {
            Self::parse_date(from)?
        } else {
            0
        };
        let to = if let Some(to) = to {
            Self::parse_date(to)?
        } else {
            chrono::Utc::now().timestamp_millis()
        };

        let mut stmt = self.conn.prepare(
            "select kind, sum(amount), count(*) from payments where requested_at between ?1 and ?2 group by kind",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![from, to], |row| {
                Ok((
                    row.get::<usize, String>(0)?,
                    row.get::<usize, i64>(1)?,
                    row.get::<usize, i64>(2)?,
                ))
            })?
            .filter_map(|r| r.ok());

        let mut stats = Stats {
            default_total: 0.0,
            default_count: 0,
            fallback_total: 0.0,
            fallback_count: 0,
        };

        for (kind, total, count) in rows {
            if kind == "default" {
                stats.default_total = (total as f64) / 100.0; // Assuming amount is stored in cents
                stats.default_count = count as usize;
            } else if kind == "fallback" {
                stats.fallback_total = (total as f64) / 100.0; // Assuming amount is stored in cents
                stats.fallback_count = count as usize;
            }
        }

        Ok(stats)
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct PaymentPost {
    #[serde(rename = "correlationId")]
    pub correlation_id: String,
    pub amount: f64,
    //example: "2025-07-15T12:34:56.000Z"
    #[serde(rename = "requestedAt")]
    pub requested_at: String,
    #[serde(skip_serializing)]
    pub requested_at_ts: i64,
}

impl PaymentPost {
    pub fn cents(&self) -> i64 {
        (self.amount * 100.0).round() as i64
    }
}

mod test {
    use crate::database::{PaymentKind, PaymentPost, PaymentsDb};

    #[allow(unused)]
    fn make_random_payment() -> PaymentPost {
        let id: String = (0..10).map(|_| fastrand::char('a'..='z')).collect();
        let requested_at = "2025-07-15T12:34:56.000Z".to_string();
        let utc_now = PaymentsDb::parse_date(&requested_at).unwrap();
        PaymentPost {
            correlation_id: id,
            amount: (fastrand::i64((0..10_000)) as f64) / 100.0,
            requested_at,
            requested_at_ts: utc_now,
        }
    }

    #[test]
    fn test_payment_post() {
        let db = PaymentsDb::new().expect("Failed to initialize database");
        let payment = make_random_payment();
        db.insert_payment(&payment, PaymentKind::Default)
            .expect("Failed to insert payment");

        let payment2 = make_random_payment();
        db.insert_payment(&payment2, PaymentKind::Default)
            .expect("Failed to insert payment");

        let stats = db
            .get_stats(
                Some("2025-07-15T00:00:00.000Z"),
                Some("2025-07-16T00:00:00.000Z"),
            )
            .expect("Failed to get range");
        assert_eq!(stats.default_count, 2);
        assert_eq!(stats.default_total, (payment.amount + payment2.amount));
    }
}

pub struct Stats {
    pub default_total: f64,
    pub default_count: usize,
    pub fallback_total: f64,
    pub fallback_count: usize,
}

impl Stats {
    pub fn to_json(&self) -> serde_json::Value {
        return serde_json::json!({
            "default":{
                "totalRequests": self.default_count,
                "totalAmount": self.default_total,
            },
            "fallback":{
                "totalRequests": self.fallback_count,
                "totalAmount": self.fallback_total,
            }
        });
    }
}
