from bs4 import BeautifulSoup
import json

with open("schedule.html", "r", encoding="utf-8") as f:
    html = f.read()

soup = BeautifulSoup(html, "html.parser")

# 找 dataTables2 —— 有课表课程列表
table = soup.find("table", id="dataTables2")
if not table:
    print("没找到 dataTables2")
    exit()

# 跳过前两行表头
rows = table.find_all("tr")[2:]

courses = []
for row in rows:
    cells = row.find_all("td")
    if len(cells) < 7:
        continue
    courses.append({
        "班级": cells[0].get_text(strip=True),
        "课程编号": cells[1].get_text(strip=True),
        "课程名": cells[2].get_text(strip=True),
        "教师": cells[3].get_text(strip=True),
        "课程性质": cells[6].get_text(strip=True),
    })

print(f"共解析出 {len(courses)} 门课：")
print(json.dumps(courses, ensure_ascii=False, indent=2))

# 存成 JSON 备用
with open("schedule.json", "w", encoding="utf-8") as f:
    json.dump(courses, f, ensure_ascii=False, indent=2)
print("\n已保存到 schedule.json")