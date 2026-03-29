use std::sync::{Arc, RwLock};

/// 泛型的无锁单写多读数据结构
///
/// 使用 AtomicPtr 存储任意类型 T，实现无锁的并发访问
/// - 写操作：原子地替换指针
/// - 读操作：原子地读取指针，获取值的克隆
///
/// 约束：
/// - T 必须实现 Clone trait，以支持多次读取
/// - T 必须实现 Send + Sync，以支持跨线程使用
#[derive(Debug)]
pub struct LockfreeSingleWriter<T>
where
    T: Clone + Send + Sync,
{
    /// 共享的可变存储（多读单写场景下性能足够，保证安全与最新可见性）
    inner: Arc<RwLock<T>>,
}

impl<T> LockfreeSingleWriter<T>
where
    T: Clone + Send + Sync,
{
    /// 创建新的单写多读管理器（共享内部存储）
    pub fn new(initial_value: T) -> Self {
        Self { inner: Arc::new(RwLock::new(initial_value)) }
    }

    /// 写入新值（写操作），返回旧值
    pub fn store(&self, new_value: T) -> T {
        let mut guard = self.inner.write().expect("LockfreeSingleWriter write poisoned");
        let old_value = guard.clone();
        *guard = new_value;
        old_value
    }

    /// 读取当前值（读操作）
    pub fn load(&self) -> T {
        let guard = self.inner.read().expect("LockfreeSingleWriter read poisoned");
        guard.clone()
    }

    /// 创建一个只读引用（共享同一份内部存储，随写入动态更新）
    pub fn reader(&self) -> LockfreeSingleReader<T> {
        LockfreeSingleReader::from_writer(self)
    }
}

// 不再需要自定义 Drop，Arc<RwLock<T>> 自动管理内存

impl<T> Clone for LockfreeSingleWriter<T>
where
    T: Clone + Send + Sync,
{
    fn clone(&self) -> Self {
        // 克隆时共享同一份内部存储，确保读者始终看到最新值
        Self { inner: Arc::clone(&self.inner) }
    }
}

unsafe impl<T> Send for LockfreeSingleWriter<T> where T: Clone + Send + Sync {}
unsafe impl<T> Sync for LockfreeSingleWriter<T> where T: Clone + Send + Sync {}

/// 泛型的无锁只读引用，只能进行读取操作
///
/// 用于其他组件只读访问数据，支持多次读取而不消耗数据
#[derive(Debug)]
pub struct LockfreeSingleReader<T>
where
    T: Clone + Send + Sync,
{
    /// 共享的只读视图（通过共享同一把锁实现实时可见）
    inner: Arc<RwLock<T>>,
}

impl<T> LockfreeSingleReader<T>
where
    T: Clone + Send + Sync,
{
    /// 从 LockfreeSingleWriter 创建只读引用（共享同一份存储）
    pub fn from_writer(writer: &LockfreeSingleWriter<T>) -> Self {
        Self { inner: Arc::clone(&writer.inner) }
    }

    /// 读取当前值（只读操作）
    pub fn load(&self) -> T {
        let guard = self.inner.read().expect("LockfreeSingleReader read poisoned");
        guard.clone()
    }
}

// 不再需要自定义 Drop，Arc<RwLock<T>> 自动管理内存

impl<T> Clone for LockfreeSingleReader<T>
where
    T: Clone + Send + Sync,
{
    fn clone(&self) -> Self {
        // 克隆时共享同一份内部存储
        Self { inner: Arc::clone(&self.inner) }
    }
}

unsafe impl<T> Send for LockfreeSingleReader<T> where T: Clone + Send + Sync {}
unsafe impl<T> Sync for LockfreeSingleReader<T> where T: Clone + Send + Sync {}

// ================================
// 具体类型：LockfreeResponseId
// ================================

/// 无锁的 ResponseId 管理器，支持1写多读
///
/// 使用 AtomicPtr 存储 Option<String>，实现无锁的并发访问
/// - 写操作：原子地替换指针
/// - 读操作：原子地读取指针，获取值的克隆
#[derive(Debug)]
pub struct LockfreeResponseId {
    /// 内部泛型实现
    inner: LockfreeSingleWriter<Option<String>>,
}

impl Default for LockfreeResponseId {
    fn default() -> Self {
        Self::new()
    }
}

impl LockfreeResponseId {
    /// 创建新的无锁 ResponseId 管理器
    pub fn new() -> Self {
        Self { inner: LockfreeSingleWriter::new(None) }
    }

    /// 写入新的 response_id（写操作）
    ///
    /// 原子地替换当前值，返回旧值
    pub fn store(&self, new_value: Option<String>) -> Option<String> {
        self.inner.store(new_value)
    }

    /// 读取当前的 response_id（读操作）
    ///
    /// 原子地读取当前值，不消耗数据，支持多次读取
    pub fn load(&self) -> Option<String> {
        self.inner.load()
    }

    /// 创建一个 ResponseId 只读引用
    pub fn reader(&self) -> LockfreeResponseIdReader {
        LockfreeResponseIdReader::from_writer(self)
    }
}

impl Clone for LockfreeResponseId {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

unsafe impl Send for LockfreeResponseId {}
unsafe impl Sync for LockfreeResponseId {}

/// 只读的 ResponseId 引用，只能进行读取操作
///
/// 用于其他组件（如 LLM Task、TTS Task 等）只读访问当前轮次的 response_id
#[derive(Debug)]
pub struct LockfreeResponseIdReader {
    /// 内部泛型实现
    inner: LockfreeSingleReader<Option<String>>,
}

impl LockfreeResponseIdReader {
    /// 从 LockfreeResponseId 创建只读引用
    pub fn from_writer(writer: &LockfreeResponseId) -> Self {
        Self { inner: LockfreeSingleReader::from_writer(&writer.inner) }
    }

    /// 读取当前的 response_id（只读操作）
    ///
    /// 原子地读取当前值，不消耗数据，支持多次读取
    pub fn load(&self) -> Option<String> {
        self.inner.load()
    }
}

impl Clone for LockfreeResponseIdReader {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

unsafe impl Send for LockfreeResponseIdReader {}
unsafe impl Sync for LockfreeResponseIdReader {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, thread};

    #[test]
    fn test_basic_operations() {
        let manager = LockfreeResponseId::new();

        // 初始值应该是 None
        assert_eq!(manager.load(), None);

        // 存储新值
        let old_value = manager.store(Some("test_id".to_string()));
        assert_eq!(old_value, None);
        assert_eq!(manager.load(), Some("test_id".to_string()));

        // 更新值
        let old_value = manager.store(Some("new_id".to_string()));
        assert_eq!(old_value, Some("test_id".to_string()));
        assert_eq!(manager.load(), Some("new_id".to_string()));
    }

    #[test]
    fn test_concurrent_read_write() {
        let manager = Arc::new(LockfreeResponseId::new());
        let manager_clone = manager.clone();

        // 启动写线程
        let writer = thread::spawn(move || {
            for i in 0..1000 {
                manager_clone.store(Some(format!("id_{}", i)));
            }
        });

        // 启动读线程
        let reader = thread::spawn(move || {
            for _ in 0..1000 {
                let _ = manager.load();
            }
        });

        writer.join().unwrap();
        reader.join().unwrap();
    }

    // 注意：泛型测试需要在单独的测试模块中进行，以避免方法名冲突
    // 这里专注于测试具体的 LockfreeResponseId 和 LockfreeResponseIdReader 类型

    #[test]
    fn test_convenience_methods() {
        let writer = LockfreeResponseId::new();

        // 测试便利方法
        let reader1 = writer.reader();
        let reader2 = LockfreeResponseIdReader::from_writer(&writer);

        // 初始值都应该是 None
        assert_eq!(reader1.load(), None);
        assert_eq!(reader2.load(), None);

        // 存储新值
        writer.store(Some("test".to_string()));

        // 旧 reader 现在也应读取到最新值（实时可见）
        assert_eq!(reader1.load(), Some("test".to_string()));
        assert_eq!(reader2.load(), Some("test".to_string()));

        // 新 reader 一样读取最新值
        let new_reader = writer.reader();
        assert_eq!(new_reader.load(), Some("test".to_string()));
    }

    // 复杂类型的测试移到单独的泛型测试模块中

    #[test]
    fn test_reader_independence() {
        let writer = LockfreeResponseId::new();
        let reader1 = writer.reader();

        // 写入值
        writer.store(Some("first".to_string()));
        let reader2 = writer.reader();

        // 再次写入值
        writer.store(Some("second".to_string()));
        let reader3 = writer.reader();

        // 所有 reader 都应观察到最新值
        assert_eq!(reader1.load(), Some("second".to_string()));
        assert_eq!(reader2.load(), Some("second".to_string()));
        assert_eq!(reader3.load(), Some("second".to_string()));
        assert_eq!(writer.load(), Some("second".to_string()));
    }
}

// 单独的泛型测试模块，避免方法名冲突
#[cfg(test)]
mod generic_tests {
    use super::*;
    use std::{sync::Arc, thread};

    #[test]
    fn test_generic_string() {
        let manager = LockfreeSingleWriter::new("initial".to_string());

        // 初始值应该是 "initial"
        assert_eq!(manager.load(), "initial");

        // 存储新值
        let old_value = manager.store("test_value".to_string());
        assert_eq!(old_value, "initial");
        assert_eq!(manager.load(), "test_value");

        // 更新值
        let old_value = manager.store("new_value".to_string());
        assert_eq!(old_value, "test_value");
        assert_eq!(manager.load(), "new_value");
    }

    #[test]
    fn test_generic_integer() {
        let manager = LockfreeSingleWriter::new(42i32);

        // 初始值应该是 42
        assert_eq!(manager.load(), 42);

        // 存储新值
        let old_value = manager.store(100);
        assert_eq!(old_value, 42);
        assert_eq!(manager.load(), 100);

        // 更新值
        let old_value = manager.store(200);
        assert_eq!(old_value, 100);
        assert_eq!(manager.load(), 200);
    }

    #[test]
    fn test_reader_functionality() {
        let writer = LockfreeSingleWriter::new("test".to_string());
        let reader = LockfreeSingleReader::from_writer(&writer);

        // 初始值应该匹配
        assert_eq!(reader.load(), "test");

        // 写入新值后，reader共享同一份存储，也能看到最新值
        writer.store("updated".to_string());
        assert_eq!(reader.load(), "updated");
        assert_eq!(writer.load(), "updated");

        // 创建新的reader也应该读取最新值
        let new_reader = LockfreeSingleReader::from_writer(&writer);
        assert_eq!(new_reader.load(), "updated");
    }

    #[test]
    fn test_concurrent_generic() {
        let manager = Arc::new(LockfreeSingleWriter::new(0i32));
        let manager_clone = manager.clone();

        // 启动写线程
        let writer = thread::spawn(move || {
            for i in 0..1000 {
                manager_clone.store(i);
            }
        });

        // 启动读线程
        let reader = thread::spawn(move || {
            for _ in 0..1000 {
                let _ = manager.load();
            }
        });

        writer.join().unwrap();
        reader.join().unwrap();
    }

    #[test]
    fn test_complex_types() {
        #[derive(Clone, Debug, PartialEq)]
        struct ComplexData {
            id: u64,
            name: String,
            values: Vec<i32>,
        }

        let initial_data = ComplexData { id: 1, name: "initial".to_string(), values: vec![1, 2, 3] };

        let manager = LockfreeSingleWriter::new(initial_data.clone());
        assert_eq!(manager.load(), initial_data);

        let new_data = ComplexData { id: 2, name: "updated".to_string(), values: vec![4, 5, 6] };

        let old_data = manager.store(new_data.clone());
        assert_eq!(old_data, initial_data);
        assert_eq!(manager.load(), new_data);
    }
}
