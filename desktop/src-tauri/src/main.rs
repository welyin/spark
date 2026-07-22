// 桌面入口：实际逻辑在 lib（与移动端共享）。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    spark_desktop_lib::run();
}
