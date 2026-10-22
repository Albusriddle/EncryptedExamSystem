fn main() {
    let a = std::path::PathBuf::from("D:/DSHworkspace/EncryptedExamSystem/.t/banks");
    let joined = a.join("many.json");
    println!("join 结果: {:?}", joined);
    println!("是否存在: {}", joined.exists());
    match std::fs::read_to_string(&joined) {
        Ok(s) => println!("读取成功，{} 字节", s.len()),
        Err(e) => println!("读取失败: {e}"),
    }
}
