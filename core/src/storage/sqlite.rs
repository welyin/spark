//! SQLite 持久化后端（移动端默认；阶段四B sqlite-backend 设计 §2）。
//!
//! - 单表 `kv(key TEXT PRIMARY KEY COLLATE BINARY, value TEXT) WITHOUT ROWID`
//!   ——COLLATE BINARY = UTF-8 字节 memcmp，与 sled 字节序/Rust str 字典序
//!   逐字节一致（契约测试的非 ASCII 键序断言原样通过）；
//! - `SqliteStorage { Arc<Mutex<Connection> }`，`Clone` 共享同一连接：单连接
//!   串行化是与 sled 内部写串行化同量级的可接受裁决（WAL 下读快照不阻塞
//!   写；不引入连接池——多连接的 SQLITE_BUSY/快照一致性是净负复杂度）；
//! - PRAGMA 组合（open 时设置）：`journal_mode=WAL`（崩溃完整性）、
//!   `synchronous=NORMAL`（WAL 下耐久性/性能平衡）、`busy_timeout=5000`
//!   （防极端竞争 SQLITE_BUSY 上抛）、`foreign_keys=OFF`；
//! - `batch` = 事务（原子提交，任一语句失败整体回滚，对齐 sled::Batch）；
//! - `scan` 先把结果集物化进 Vec 再出锁（SQL 游标不留存，与 sled 物化
//!   语义一致——持锁期间不堵写长尾）。
//!
//! 实现注记（与设计 §2.4 的偏差，交付报告已记）：设计选 rusqlite 包装层，
//! 实施环境离线拉取不到 rusqlite 本体（其 bundled 底层 libsqlite3-sys 与
//! 部分依赖已在本地缓存）——降级为直用 libsqlite3-sys + 本模块自持的薄
//! 同步封装（FFI 面收敛在 `Connection`/`Statement` 两个内部类型）。bundled
//! 编译/无系统库/无 libclang 的选型理由不受影响。

use std::ffi::CString;
use std::path::Path;
use std::sync::{Arc, Mutex};

use libsqlite3_sys as ffi;

use super::{BatchOperation, Result, ScanOptions, StorageBackend, StorageError};

fn backend_err(context: &str, e: impl std::fmt::Display) -> StorageError {
    StorageError::Backend(format!("sqlite {context}: {e}"))
}

/// C 宏 `SQLITE_TRANSIENT`（`(sqlite3_destructor_type)-1`：SQLite 内部复制
/// 绑定文本，不持有调用方缓冲）——绑定层不暴露宏，按 rusqlite 惯例自定义。
/// 运行期函数而非 const：函数指针类型的 const 求值过不了 rustc 的 UB 检查。
fn sqlite_transient() -> ffi::sqlite3_destructor_type {
    // 安全说明：-1 是该 API 的哨兵约定值，非真实函数指针（从不被调用）。
    unsafe { std::mem::transmute(-1isize) }
}

/// 读 sqlite 错误消息（C 串 → String；db 为 null 时给静态文案）。
///
/// # Safety
/// `db` 须为 sqlite3_open 系列返回的句柄或 null。
unsafe fn errmsg(db: *mut ffi::sqlite3) -> String {
    if db.is_null() {
        return "out of memory".to_string();
    }
    let ptr = unsafe { ffi::sqlite3_errmsg(db) };
    if ptr.is_null() {
        return "unknown error".to_string();
    }
    unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

/// SQLite 连接句柄的薄封装（内部类型，FFI 边界）。
///
/// 线程安全依据：打开时 `SQLITE_OPEN_FULLMUTEX`（SQLite 自身串行化）+
/// 外层 `Arc<Mutex>` 串行化全部调用；`Send` 成立（跨线程移动），`Sync`
/// 由外层 Mutex 提供。
struct Connection {
    db: *mut ffi::sqlite3,
}

// 安全说明：见上（FULLMUTEX + 外层 Mutex 串行化；句柄可跨线程移动）。
unsafe impl Send for Connection {}

impl Connection {
    fn open(path: &Path) -> Result<Self> {
        let path_bytes = path.as_os_str().as_encoded_bytes();
        let c_path = CString::new(path_bytes).map_err(|e| backend_err("open path", e))?;
        let mut db = std::ptr::null_mut();
        // 安全说明：c_path 在本调用内有效；db 由 SQLite 写回。
        let rc = unsafe {
            ffi::sqlite3_open_v2(
                c_path.as_ptr(),
                &mut db,
                ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE | ffi::SQLITE_OPEN_FULLMUTEX,
                std::ptr::null(),
            )
        };
        if rc != ffi::SQLITE_OK {
            // 安全说明：db 可能非 null 半成品句柄，读错误消息后关闭。
            let msg = unsafe { errmsg(db) };
            if !db.is_null() {
                unsafe { ffi::sqlite3_close(db) };
            }
            return Err(backend_err("open", format!("rc={rc} {msg}")));
        }
        Ok(Self { db })
    }

    /// 执行无返回行的 SQL 批（PRAGMA/DDL/事务控制）。
    fn exec_batch(&self, context: &str, sql: &str) -> Result<()> {
        let c_sql = CString::new(sql).map_err(|e| backend_err(context, e))?;
        // 安全说明：句柄有效；c_sql 在本调用内有效；回调与错误缓冲均置空。
        let rc = unsafe {
            ffi::sqlite3_exec(
                self.db,
                c_sql.as_ptr(),
                None,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if rc != ffi::SQLITE_OK {
            // 安全说明：句柄有效。
            return Err(backend_err(context, unsafe { errmsg(self.db) }));
        }
        Ok(())
    }

    /// 预编译语句并绑定参数。
    fn prepare(&self, sql: &str, params: &[BindParam<'_>]) -> Result<Statement> {
        let c_sql = CString::new(sql).map_err(|e| backend_err("prepare", e))?;
        let mut stmt = std::ptr::null_mut();
        // 安全说明：句柄有效；c_sql 在本调用内有效；语句句柄由 SQLite 写回，
        // 由 Statement 的 Drop（finalize）释放。
        let rc = unsafe {
            ffi::sqlite3_prepare_v2(self.db, c_sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut())
        };
        if rc != ffi::SQLITE_OK {
            return Err(backend_err("prepare", unsafe { errmsg(self.db) }));
        }
        let stmt = Statement { stmt };
        for (idx, param) in params.iter().enumerate() {
            stmt.bind(idx as i32 + 1, param)?;
        }
        Ok(stmt)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if !self.db.is_null() {
            // 安全说明：句柄有效；所有语句均已 finalize（Statement Drop）。
            unsafe { ffi::sqlite3_close(self.db) };
        }
    }
}

/// 语句绑定参数（kv 全是 TEXT，外加 scan 的 LIMIT 整数）。
enum BindParam<'a> {
    Text(&'a str),
    Int(i64),
}

/// 预编译语句的薄封装（Drop 时 finalize）。
struct Statement {
    stmt: *mut ffi::sqlite3_stmt,
}

impl Statement {
    fn bind(&self, index: i32, param: &BindParam<'_>) -> Result<()> {
        // 安全说明：语句句柄有效；文本以 SQLITE_TRANSIENT 绑定（SQLite 内部
        // 复制，不持有调用方缓冲），切片在本调用内有效。
        let rc = unsafe {
            match param {
                BindParam::Text(s) => ffi::sqlite3_bind_text(
                    self.stmt,
                    index,
                    s.as_ptr() as *const std::os::raw::c_char,
                    s.len() as i32,
                    sqlite_transient(),
                ),
                BindParam::Int(v) => ffi::sqlite3_bind_int64(self.stmt, index, *v),
            }
        };
        if rc != ffi::SQLITE_OK {
            return Err(backend_err("bind", format!("rc={rc}")));
        }
        Ok(())
    }

    /// 步进：ROW → Some(（第 0 列文本， 第 1 列文本）)；DONE → None。
    fn step_row(&self) -> Result<Option<(String, String)>> {
        // 安全说明：语句句柄有效。
        let rc = unsafe { ffi::sqlite3_step(self.stmt) };
        match rc {
            ffi::SQLITE_ROW => Ok(Some((self.column_text(0)?, self.column_text(1)?))),
            ffi::SQLITE_DONE => Ok(None),
            _ => Err(backend_err("step", format!("rc={rc}"))),
        }
    }

    /// 读单列文本（get 点查用）。
    fn step_value(&self) -> Result<Option<String>> {
        // 安全说明：语句句柄有效。
        let rc = unsafe { ffi::sqlite3_step(self.stmt) };
        match rc {
            ffi::SQLITE_ROW => Ok(Some(self.column_text(0)?)),
            ffi::SQLITE_DONE => Ok(None),
            _ => Err(backend_err("step", format!("rc={rc}"))),
        }
    }

    /// 执行写语句（put/delete），返回变更行数（未用，校验执行成功即可）。
    fn execute(&self) -> Result<()> {
        // 安全说明：语句句柄有效。
        let rc = unsafe { ffi::sqlite3_step(self.stmt) };
        match rc {
            ffi::SQLITE_DONE | ffi::SQLITE_ROW => Ok(()),
            _ => Err(backend_err("execute", format!("rc={rc}"))),
        }
    }

    /// 读第 `col` 列文本（列指针 + 字节数成对取，允许内嵌 NUL 之外的任意
    /// UTF-8；本库键值均为合法 UTF-8 String）。
    fn column_text(&self, col: i32) -> Result<String> {
        // 安全说明：语句句柄有效且停在 ROW 上；指针/长度由 SQLite 持有，
        // 在下一次 step/finalize 前有效——此处立即复制出切片。
        let (ptr, len) = unsafe {
            (
                ffi::sqlite3_column_text(self.stmt, col),
                ffi::sqlite3_column_bytes(self.stmt, col),
            )
        };
        if ptr.is_null() {
            return Ok(String::new()); // NULL/空值 → 空串（kv.value NOT NULL，防御）
        }
        let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
        String::from_utf8(bytes.to_vec()).map_err(|e| backend_err("column utf8", e))
    }
}

impl Drop for Statement {
    fn drop(&mut self) {
        if !self.stmt.is_null() {
            // 安全说明：语句句柄有效。
            unsafe { ffi::sqlite3_finalize(self.stmt) };
        }
    }
}

/// SQLite 持久化后端。
#[derive(Clone)]
pub struct SqliteStorage {
    conn: Arc<Mutex<Connection>>,
    /// 测试故障注入：下一次 batch 在 COMMIT 前人为失败（验证事务整体回滚）。
    /// kv 单表无约束可触发失败（INSERT OR REPLACE 恒成功），设计 §4 允许
    /// 「以可构造的失败为准」。
    #[cfg(test)]
    fail_next_batch: Arc<std::sync::atomic::AtomicBool>,
}

impl SqliteStorage {
    /// 打开（或创建）指定文件的 SQLite 数据库（WAL 伴生 `-wal`/`-shm`）。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path.as_ref())?;
        conn.exec_batch(
            "pragma",
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; \
             PRAGMA busy_timeout=5000; PRAGMA foreign_keys=OFF;",
        )?;
        conn.exec_batch(
            "create table",
            "CREATE TABLE IF NOT EXISTS kv (
               key   TEXT PRIMARY KEY COLLATE BINARY,
               value TEXT NOT NULL
             ) WITHOUT ROWID;",
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            #[cfg(test)]
            fail_next_batch: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    /// 刷盘（kernel shutdown 时调用，对齐 sled 路径的确定性收尾）：WAL
    /// 被动 checkpoint（synchronous=NORMAL 下 commit 已保证崩溃完整性，
    /// checkpoint 只是把 WAL 收拢回主库文件）。
    pub fn flush(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.exec_batch("wal_checkpoint", "PRAGMA wal_checkpoint=PASSIVE")
    }

    /// 当前键值对数量（诊断用）。
    pub fn len(&self) -> usize {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.prepare("SELECT COUNT(*) FROM kv", &[])
            .and_then(|stmt| stmt.step_value())
            .ok()
            .flatten()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0)
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::fmt::Debug for SqliteStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteStorage")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

impl StorageBackend for SqliteStorage {
    fn get(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.prepare(
            "SELECT value FROM kv WHERE key = ?1",
            &[BindParam::Text(key)],
        )?
        .step_value()
    }

    fn put(&mut self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.prepare(
            "INSERT OR REPLACE INTO kv (key, value) VALUES (?1, ?2)",
            &[BindParam::Text(key), BindParam::Text(value)],
        )?
        .execute()
    }

    fn delete(&mut self, key: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // 不存在不报错（DELETE 无匹配行非错误），与 LevelDB del 语义一致
        conn.prepare("DELETE FROM kv WHERE key = ?1", &[BindParam::Text(key)])?
            .execute()
    }

    fn batch(&mut self, operations: Vec<BatchOperation>) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.exec_batch("batch begin", "BEGIN IMMEDIATE")?;
        let applied = (|| -> Result<()> {
            for op in operations {
                match op {
                    BatchOperation::Put { key, value } => conn
                        .prepare(
                            "INSERT OR REPLACE INTO kv (key, value) VALUES (?1, ?2)",
                            &[BindParam::Text(&key), BindParam::Text(&value)],
                        )?
                        .execute()?,
                    BatchOperation::Delete { key } => conn
                        .prepare("DELETE FROM kv WHERE key = ?1", &[BindParam::Text(&key)])?
                        .execute()?,
                }
            }
            Ok(())
        })();
        #[cfg(test)]
        let applied = applied.and_then(|()| {
            if self
                .fail_next_batch
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(backend_err("batch apply", "injected failure"));
            }
            Ok(())
        });
        match applied {
            Ok(()) => conn.exec_batch("batch commit", "COMMIT"),
            Err(e) => {
                // 回滚失败只叠加日志语义：原错误优先返回
                let _ = conn.exec_batch("batch rollback", "ROLLBACK");
                Err(e)
            }
        }
    }

    fn scan(&self, options: &ScanOptions) -> Result<Vec<(String, String)>> {
        let start = options.resolved_start();
        let end = options.resolved_end();
        // start >= end（含相等——[s, s) 恒空）与 limit=0 直接返回空，不进 SQL
        // （与 MemoryStorage 的防御一致）
        if start >= end || options.limit == Some(0) {
            return Ok(Vec::new());
        }
        let sql = if options.reverse {
            "SELECT key, value FROM kv WHERE key >= ?1 AND key < ?2 ORDER BY key DESC LIMIT ?3"
        } else {
            "SELECT key, value FROM kv WHERE key >= ?1 AND key < ?2 ORDER BY key ASC LIMIT ?3"
        };
        let limit = options
            .limit
            .map(|n| n.min(i64::MAX as usize) as i64)
            .unwrap_or(-1); // SQLite LIMIT -1 = 不限
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let stmt = conn.prepare(
            sql,
            &[
                BindParam::Text(&start),
                BindParam::Text(&end),
                BindParam::Int(limit),
            ],
        )?;
        // 物化进 Vec 再出锁（游标不留存，与 sled 物化语义一致）
        let mut items = Vec::new();
        while let Some(row) = stmt.step_row()? {
            items.push(row);
        }
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::contract::check_storage_contract;

    /// 契约（sqlite-backend §4）：与 Memory/sled 同一组语义断言逐条过
    /// （含非 ASCII 键、reverse+limit、空范围、limit=0）。
    #[test]
    fn sqlite_passes_storage_contract() {
        let dir = tempfile::tempdir().unwrap();
        let mut storage = SqliteStorage::open(dir.path().join("t.db")).unwrap();
        check_storage_contract(&mut storage);
    }

    /// 持久化：写入后关库重开读回（WAL 模式——不写 checkpoint，数据在
    /// `-wal` 伴生文件中，重开须照样可读）。
    #[test]
    fn persistence_across_reopen_including_uncheckpointed_wal() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("t.db");
        {
            let mut s = SqliteStorage::open(&file).unwrap();
            s.put("doc:a:1", "v1").unwrap();
            s.batch(vec![
                BatchOperation::put("doc:a:2", "v2"),
                BatchOperation::put("meta:a:1", "{\"ts\":1}"),
            ])
            .unwrap();
            // 不 flush——WAL 未 checkpoint，重开读回验证 WAL 恢复
        }
        let s = SqliteStorage::open(&file).unwrap();
        assert_eq!(s.get("doc:a:1").unwrap(), Some("v1".to_string()));
        assert_eq!(s.scan(&ScanOptions::prefix("doc:a:")).unwrap().len(), 2);
        assert_eq!(s.get("meta:a:1").unwrap(), Some("{\"ts\":1}".to_string()));
    }

    /// Clone 共享同一连接（Mutex 共享语义）：一句柄写另一句柄读。
    #[test]
    fn clone_shares_same_connection() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = SqliteStorage::open(dir.path().join("t.db")).unwrap();
        let b = a.clone();
        a.put("k", "v").unwrap();
        assert_eq!(b.get("k").unwrap(), Some("v".to_string()));
    }

    /// batch 原子性：COMMIT 前人为失败（测试故障注入——kv 单表无约束可
    /// 构造真实失败，设计 §4 允许以可构造的失败为准）→ 整体回滚。
    #[test]
    fn batch_rolls_back_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = SqliteStorage::open(dir.path().join("t.db")).unwrap();
        s.put("base", "0").unwrap();
        s.fail_next_batch
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let r = s.batch(vec![
            BatchOperation::put("a", "1"),
            BatchOperation::put("b", "2"),
            BatchOperation::delete("base"),
        ]);
        assert!(r.is_err(), "故障注入使 batch 失败");
        assert_eq!(s.get("a").unwrap(), None, "失败 batch 的写入回滚");
        assert_eq!(s.get("b").unwrap(), None);
        assert_eq!(s.get("base").unwrap(), Some("0".to_string()), "删除同回滚");
    }

    /// 并发冒烟：多线程各持 Clone 句柄混合读写共 1k 次——无 SQLITE_BUSY
    /// 上抛（单连接 Mutex 串行化 + busy_timeout 生效）。
    #[test]
    fn concurrent_clone_handles_no_busy_errors() {
        let dir = tempfile::tempdir().unwrap();
        let s = SqliteStorage::open(dir.path().join("t.db")).unwrap();
        let mut handles = Vec::new();
        for t in 0..4 {
            let mut h = s.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..250 {
                    let key = format!("k:{}", (t * 250 + i) % 100);
                    h.put(&key, "v").unwrap();
                    let _ = h.get(&key).unwrap();
                    if i % 10 == 0 {
                        let _ = h.scan(&ScanOptions::prefix("k:")).unwrap();
                    }
                    if i % 25 == 0 {
                        h.batch(vec![
                            BatchOperation::put(format!("b:{t}:{i}"), "x"),
                            BatchOperation::delete(format!("b:{t}:none")),
                        ])
                        .unwrap();
                    }
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(s.len(), 100 + 4 * 10, "全部写入可见");
    }
}
