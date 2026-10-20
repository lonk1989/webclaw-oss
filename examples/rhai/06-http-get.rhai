/// 用法：脚本示例HTTP GET [可选URL]
/// 演示 http_get + json_parse，把响应落到 files/
let url = arg(0, "https://jsonplaceholder.typicode.com/todos/1");
print("GET " + url);

let raw = http_get(url);
let obj = json_parse(raw);

print("id=" + to_str(obj["id"]) + "  title=" + to_str(obj["title"]));
print("completed=" + to_str(obj["completed"]));

let out = "files/http-get-demo.json";
write_text_file(out, raw);
track_output(out);
print("✓ 已保存 " + out);
