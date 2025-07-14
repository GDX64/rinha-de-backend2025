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

impl PaymentsDb {
    pub fn new() -> anyhow::Result<Self> {
        let conn = rusqlite::Connection::open_in_memory()?;

        conn.execute(
            "create table if not exists payments (
                  uuid text primary key,
                  amount real not null,
                  requested_at timestamp not null,
                  kind text not null
             )",
            rusqlite::params![],
        )?;
        Ok(Self { conn })
    }

    pub fn insert_payment(&self, post: &PaymentPost, kind: &str) -> anyhow::Result<()> {
        let requested_at = Self::parse_date(&post.requested_at)?;
        self.conn.execute(
            "insert into payments (uuid, amount, requested_at, kind) values (?1, ?2, ?3, ?4)",
            rusqlite::params![post.correlation_id, post.amount, requested_at, kind],
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

    pub fn get_range(&self, from: &str, to: &str) -> anyhow::Result<Vec<PaymentPost>> {
        let from = Self::parse_date(from)?;
        let to = Self::parse_date(to)?;

        let mut stmt = self.conn.prepare(
            "select uuid, amount, requested_at from payments where requested_at between ?1 and ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![from, to], |row| {
            Ok(PaymentPost {
                correlation_id: row.get(0)?,
                amount: row.get(1)?,
                requested_at: Self::date_from_millis(row.get::<usize, i64>(2)?),
            })
        })?;

        let mut payments = Vec::new();
        for payment in rows {
            payments.push(payment?);
        }
        Ok(payments)
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
}

mod test {
    use crate::database::{PaymentPost, PaymentsDb};

    #[test]
    fn test_payment_post() {
        let db = PaymentsDb::new().expect("Failed to initialize database");
        let payment = PaymentPost {
            correlation_id: "12345".to_string(),
            amount: 100.0,
            requested_at: "2025-07-15T12:34:56.000Z".to_string(),
        };
        db.insert_payment(&payment, "default")
            .expect("Failed to insert payment");

        let payments = db
            .get_range("2025-07-15T00:00:00.000Z", "2025-07-16T00:00:00.000Z")
            .expect("Failed to get range");
        assert_eq!(payments.get(0).unwrap(), &payment);
    }
}
