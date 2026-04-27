//! Database persistence layer.

use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;

use crate::cache::CacheSnapshot;
use crate::engine::account::{Account, AccountId, Position};
use crate::engine::orders::Order;
use crate::messages::{ClientOrderId, PositionId, TraderId};

// =============================================================================
// DatabaseError
// =============================================================================

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Database not open")]
    NotOpen,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, DatabaseError>;

// =============================================================================
// Database trait
// =============================================================================

pub trait Database: Send + Sync {
    // Orders
    fn save_order(&self, order: &Order) -> Result<()>;
    fn load_orders(&self) -> Result<HashMap<ClientOrderId, Order>>;

    // Positions
    fn save_position(&self, position: &Position) -> Result<()>;
    fn load_positions(&self) -> Result<HashMap<PositionId, Position>>;

    // Instruments (stub)
    fn save_instrument(&self, instrument_id: &str, data: &[u8]) -> Result<()>;
    fn load_instrument(&self, instrument_id: &str) -> Result<Option<Vec<u8>>>;

    // Accounts
    fn save_account(&self, account: &Account) -> Result<()>;
    fn load_accounts(&self) -> Result<HashMap<AccountId, Account>>;

    // Strategy state
    fn save_strategy_state(&self, strategy_id: &str, state: &[u8]) -> Result<()>;
    fn load_strategy_state(&self, strategy_id: &str) -> Result<Option<Vec<u8>>>;

    // Equity curve snapshots
    fn save_equity_curve(&self, snapshot: &CacheSnapshot) -> Result<()>;
    fn load_equity_curve(&self) -> Result<Vec<CacheSnapshot>>;

    // Heartbeat
    fn save_heartbeat(&self, trader_id: &TraderId, ts_ns: u64) -> Result<()>;
    fn load_heartbeat(&self, trader_id: &TraderId) -> Result<Option<u64>>;

    // Transaction management
    fn flush(&self) -> Result<()>;
    fn close(&self) -> Result<()>;
}

// =============================================================================
// SqliteDatabase
// =============================================================================

use rusqlite::Connection;
use std::sync::Mutex;

pub struct SqliteDatabase {
    conn: Mutex<Option<Connection>>,
    path: PathBuf,
}

impl SqliteDatabase {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            conn: Mutex::new(None),
            path: path.into(),
        }
    }

    pub fn open(&self) -> Result<()> {
        let conn = Connection::open(&self.path)?;
        self.init_schema(&conn)?;
        *self.conn.lock().unwrap() = Some(conn);
        Ok(())
    }

    fn init_schema(&self, conn: &Connection) -> Result<()> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS orders (
                client_order_id TEXT PRIMARY KEY,
                data TEXT NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS positions (
                position_id TEXT PRIMARY KEY,
                data TEXT NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS accounts (
                account_id TEXT PRIMARY KEY,
                data TEXT NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS equity_curves (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp_ns INTEGER NOT NULL,
                data TEXT NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS heartbeats (
                trader_id TEXT PRIMARY KEY,
                ts_ns INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS instruments (
                instrument_id TEXT PRIMARY KEY,
                data BLOB NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS strategy_state (
                strategy_id TEXT PRIMARY KEY,
                data BLOB NOT NULL
            )",
            [],
        )?;
        Ok(())
    }

    fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let conn_guard = self.conn.lock().unwrap();
        let conn = conn_guard.as_ref().ok_or(DatabaseError::NotOpen)?;
        f(conn)
    }
}

impl Database for SqliteDatabase {
    fn save_order(&self, order: &Order) -> Result<()> {
        self.with_conn(|conn| {
            let data = serde_json::to_string(order).map_err(|e| DatabaseError::Serialization(e.to_string()))?;
            conn.execute(
                "INSERT OR REPLACE INTO orders (client_order_id, data) VALUES (?1, ?2)",
                rusqlite::params![order.client_order_id.to_string(), data],
            )?;
            Ok(())
        })
    }

    fn load_orders(&self) -> Result<HashMap<ClientOrderId, Order>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT data FROM orders")?;
            let rows = stmt.query_map([], |row| {
                let data: String = row.get(0)?;
                Ok(data)
            })?;
            let mut map = HashMap::new();
            for row in rows {
                let data = row?;
                let order: Order = serde_json::from_str(&data)
                    .map_err(|e| DatabaseError::Serialization(e.to_string()))?;
                map.insert(order.client_order_id.clone(), order);
            }
            Ok(map)
        })
    }

    fn save_position(&self, position: &Position) -> Result<()> {
        self.with_conn(|conn| {
            let data = serde_json::to_string(position).map_err(|e| DatabaseError::Serialization(e.to_string()))?;
            conn.execute(
                "INSERT OR REPLACE INTO positions (position_id, data) VALUES (?1, ?2)",
                rusqlite::params![position.position_id.to_string(), data],
            )?;
            Ok(())
        })
    }

    fn load_positions(&self) -> Result<HashMap<PositionId, Position>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT data FROM positions")?;
            let rows = stmt.query_map([], |row| {
                let data: String = row.get(0)?;
                Ok(data)
            })?;
            let mut map = HashMap::new();
            for row in rows {
                let data = row?;
                let position: Position = serde_json::from_str(&data)
                    .map_err(|e| DatabaseError::Serialization(e.to_string()))?;
                map.insert(position.position_id.clone(), position);
            }
            Ok(map)
        })
    }

    fn save_account(&self, account: &Account) -> Result<()> {
        self.with_conn(|conn| {
            let data = serde_json::to_string(account).map_err(|e| DatabaseError::Serialization(e.to_string()))?;
            conn.execute(
                "INSERT OR REPLACE INTO accounts (account_id, data) VALUES (?1, ?2)",
                rusqlite::params![account.id.to_string(), data],
            )?;
            Ok(())
        })
    }

    fn load_accounts(&self) -> Result<HashMap<AccountId, Account>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT data FROM accounts")?;
            let rows = stmt.query_map([], |row| {
                let data: String = row.get(0)?;
                Ok(data)
            })?;
            let mut map = HashMap::new();
            for row in rows {
                let data = row?;
                let account: Account = serde_json::from_str(&data)
                    .map_err(|e| DatabaseError::Serialization(e.to_string()))?;
                map.insert(account.id.clone(), account);
            }
            Ok(map)
        })
    }

    fn save_equity_curve(&self, snapshot: &CacheSnapshot) -> Result<()> {
        self.with_conn(|conn| {
            let data = serde_json::to_string(snapshot).map_err(|e| DatabaseError::Serialization(e.to_string()))?;
            conn.execute(
                "INSERT INTO equity_curves (timestamp_ns, data) VALUES (?1, ?2)",
                rusqlite::params![snapshot.timestamp_ns as i64, data],
            )?;
            Ok(())
        })
    }

    fn load_equity_curve(&self) -> Result<Vec<CacheSnapshot>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT data FROM equity_curves ORDER BY id ASC")?;
            let rows = stmt.query_map([], |row| {
                let data: String = row.get(0)?;
                Ok(data)
            })?;
            let mut snapshots = Vec::new();
            for row in rows {
                let data = row?;
                let snapshot: CacheSnapshot = serde_json::from_str(&data)
                    .map_err(|e| DatabaseError::Serialization(e.to_string()))?;
                snapshots.push(snapshot);
            }
            Ok(snapshots)
        })
    }

    fn save_heartbeat(&self, trader_id: &TraderId, ts_ns: u64) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO heartbeats (trader_id, ts_ns) VALUES (?1, ?2)",
                rusqlite::params![trader_id.as_str(), ts_ns as i64],
            )?;
            Ok(())
        })
    }

    fn load_heartbeat(&self, trader_id: &TraderId) -> Result<Option<u64>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT ts_ns FROM heartbeats WHERE trader_id = ?1")?;
            let result = stmt.query_row(rusqlite::params![trader_id.as_str()], |row| {
                let ts_ns: i64 = row.get(0)?;
                Ok(ts_ns as u64)
            });
            match result {
                Ok(ts_ns) => Ok(Some(ts_ns)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e)?,
            }
        })
    }

    fn save_instrument(&self, instrument_id: &str, data: &[u8]) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO instruments (instrument_id, data) VALUES (?1, ?2)",
                rusqlite::params![instrument_id, data],
            )?;
            Ok(())
        })
    }

    fn load_instrument(&self, instrument_id: &str) -> Result<Option<Vec<u8>>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT data FROM instruments WHERE instrument_id = ?1")?;
            let mut rows = stmt.query(rusqlite::params![instrument_id])?;
            if let Some(row) = rows.next()? {
                let data: Vec<u8> = row.get(0)?;
                Ok(Some(data))
            } else {
                Ok(None)
            }
        })
    }

    fn save_strategy_state(&self, strategy_id: &str, state: &[u8]) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO strategy_state (strategy_id, data) VALUES (?1, ?2)",
                rusqlite::params![strategy_id, state],
            )?;
            Ok(())
        })
    }

    fn load_strategy_state(&self, strategy_id: &str) -> Result<Option<Vec<u8>>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT data FROM strategy_state WHERE strategy_id = ?1")?;
            let mut rows = stmt.query(rusqlite::params![strategy_id])?;
            if let Some(row) = rows.next()? {
                let data: Vec<u8> = row.get(0)?;
                Ok(Some(data))
            } else {
                Ok(None)
            }
        })
    }

    fn flush(&self) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
    }

    fn close(&self) -> Result<()> {
        let mut conn_guard = self.conn.lock().unwrap();
        *conn_guard = None;
        Ok(())
    }
}

unsafe impl Send for SqliteDatabase {}
unsafe impl Sync for SqliteDatabase {}

// =============================================================================
// MemoryDatabase
// =============================================================================

pub struct MemoryDatabase {
    orders: Mutex<HashMap<ClientOrderId, Order>>,
    positions: Mutex<HashMap<PositionId, Position>>,
    accounts: Mutex<HashMap<AccountId, Account>>,
    equity_curves: Mutex<Vec<CacheSnapshot>>,
    heartbeat: Mutex<HashMap<String, u64>>,
    instruments: Mutex<HashMap<String, Vec<u8>>>,
    strategy_state: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemoryDatabase {
    pub fn new() -> Self {
        Self {
            orders: Mutex::new(HashMap::new()),
            positions: Mutex::new(HashMap::new()),
            accounts: Mutex::new(HashMap::new()),
            equity_curves: Mutex::new(Vec::new()),
            heartbeat: Mutex::new(HashMap::new()),
            instruments: Mutex::new(HashMap::new()),
            strategy_state: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for MemoryDatabase {
    fn default() -> Self {
        Self::new()
    }
}

impl Database for MemoryDatabase {
    fn save_order(&self, order: &Order) -> Result<()> {
        self.orders.lock().unwrap().insert(order.client_order_id.clone(), order.clone());
        Ok(())
    }

    fn load_orders(&self) -> Result<HashMap<ClientOrderId, Order>> {
        Ok(self.orders.lock().unwrap().clone())
    }

    fn save_position(&self, position: &Position) -> Result<()> {
        self.positions.lock().unwrap().insert(position.position_id.clone(), position.clone());
        Ok(())
    }

    fn load_positions(&self) -> Result<HashMap<PositionId, Position>> {
        Ok(self.positions.lock().unwrap().clone())
    }

    fn save_account(&self, account: &Account) -> Result<()> {
        self.accounts.lock().unwrap().insert(account.id.clone(), account.clone());
        Ok(())
    }

    fn load_accounts(&self) -> Result<HashMap<AccountId, Account>> {
        Ok(self.accounts.lock().unwrap().clone())
    }

    fn save_equity_curve(&self, snapshot: &CacheSnapshot) -> Result<()> {
        self.equity_curves.lock().unwrap().push(snapshot.clone());
        Ok(())
    }

    fn load_equity_curve(&self) -> Result<Vec<CacheSnapshot>> {
        Ok(self.equity_curves.lock().unwrap().clone())
    }

    fn save_heartbeat(&self, trader_id: &TraderId, ts_ns: u64) -> Result<()> {
        self.heartbeat.lock().unwrap().insert(trader_id.0.clone(), ts_ns);
        Ok(())
    }

    fn load_heartbeat(&self, trader_id: &TraderId) -> Result<Option<u64>> {
        Ok(self.heartbeat.lock().unwrap().get(&trader_id.0).copied())
    }

    fn save_instrument(&self, instrument_id: &str, data: &[u8]) -> Result<()> {
        self.instruments.lock().unwrap().insert(instrument_id.to_string(), data.to_vec());
        Ok(())
    }

    fn load_instrument(&self, instrument_id: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.instruments.lock().unwrap().get(instrument_id).cloned())
    }

    fn save_strategy_state(&self, strategy_id: &str, state: &[u8]) -> Result<()> {
        self.strategy_state.lock().unwrap().insert(strategy_id.to_string(), state.to_vec());
        Ok(())
    }

    fn load_strategy_state(&self, strategy_id: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.strategy_state.lock().unwrap().get(strategy_id).cloned())
    }

    fn flush(&self) -> Result<()> {
        Ok(())
    }

    fn close(&self) -> Result<()> {
        Ok(())
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_save_load_orders_roundtrip() {
        let db = MemoryDatabase::new();
        let order = Order::new(
            1,
            ClientOrderId::new("test-order-1"),
            crate::messages::StrategyId::new("test-strategy"),
            crate::instrument::InstrumentId::new("BTCUSDT", "BINANCE"),
            crate::instrument::Venue::new("BINANCE"),
            crate::engine::orders::OrderSide::Buy,
            crate::engine::orders::OrderType::Market,
            50_000.0,
            1.0,
            None,
            None,
        );
        db.save_order(&order).unwrap();
        let loaded = db.load_orders().unwrap();
        assert_eq!(loaded.len(), 1);
        let loaded_order = loaded.get(&ClientOrderId::new("test-order-1")).unwrap();
        assert_eq!(loaded_order.id, 1);
        assert_eq!(loaded_order.price, 50_000.0);
    }

    #[test]
    fn test_save_load_positions_roundtrip() {
        let db = MemoryDatabase::new();
        let position = Position::new(
            PositionId::new("pos-1"),
            crate::instrument::InstrumentId::new("BTCUSDT", "BINANCE"),
            crate::messages::StrategyId::new("test-strategy"),
            crate::instrument::Venue::new("BINANCE"),
            crate::messages::OrderSide::Buy,
            1.0,
            50_000.0,
            0,
        );
        db.save_position(&position).unwrap();
        let loaded = db.load_positions().unwrap();
        assert_eq!(loaded.len(), 1);
        let loaded_pos = loaded.get(&PositionId::new("pos-1")).unwrap();
        assert_eq!(loaded_pos.quantity, 1.0);
    }

    #[test]
    fn test_save_load_equity_curve() {
        let db = MemoryDatabase::new();
        let snap = CacheSnapshot {
            orders: HashMap::new(),
            positions: HashMap::new(),
            accounts: HashMap::new(),
            timestamp_ns: 12345,
        };
        db.save_equity_curve(&snap).unwrap();
        db.save_equity_curve(&snap).unwrap();
        let loaded = db.load_equity_curve().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].timestamp_ns, 12345);
    }

    #[test]
    fn test_save_load_heartbeat() {
        let db = MemoryDatabase::new();
        let trader_id = TraderId::new("TRADER-001");
        db.save_heartbeat(&trader_id, 999).unwrap();
        let loaded = db.load_heartbeat(&trader_id).unwrap();
        assert_eq!(loaded, Some(999));
    }

    #[test]
    fn test_memory_database_all_methods() {
        let db = MemoryDatabase::new();
        // Verify instrument storage
        assert!(db.save_instrument("inst1", &[1, 2, 3]).is_ok());
        assert_eq!(db.load_instrument("inst1").unwrap(), Some(vec![1, 2, 3]));
        // Verify strategy state storage
        assert!(db.save_strategy_state("strat1", &[4, 5, 6]).is_ok());
        assert_eq!(db.load_strategy_state("strat1").unwrap(), Some(vec![4, 5, 6]));
        // Verify missing keys return None
        assert!(db.load_instrument("missing").unwrap().is_none());
        assert!(db.load_strategy_state("missing").unwrap().is_none());
        assert!(db.flush().is_ok());
        assert!(db.close().is_ok());
    }

    #[test]
    fn test_sqlite_database_persists_to_disk() {
        use std::fs;
        let temp_path = format!("/tmp/test_nexus_{}.db", std::process::id());
        let _ = fs::remove_file(&temp_path);

        let db = SqliteDatabase::new(&temp_path);
        db.open().unwrap();

        let order = Order::new(
            1,
            ClientOrderId::new("sqlite-test"),
            crate::messages::StrategyId::new("test-strategy"),
            crate::instrument::InstrumentId::new("BTCUSDT", "BINANCE"),
            crate::instrument::Venue::new("BINANCE"),
            crate::engine::orders::OrderSide::Buy,
            crate::engine::orders::OrderType::Market,
            50_000.0,
            1.0,
            None,
            None,
        );
        db.save_order(&order).unwrap();
        db.flush().unwrap();

        // Reopen
        drop(db);
        let db2 = SqliteDatabase::new(&temp_path);
        db2.open().unwrap();
        let loaded = db2.load_orders().unwrap();
        assert_eq!(loaded.len(), 1);
        let _ = fs::remove_file(&temp_path);
    }
}
