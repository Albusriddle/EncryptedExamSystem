"""项目状态：UI 与 core 之间的唯一数据容器。

## 为什么需要这一层

四个 Tab 共享同一份数据（题库、学生、场次、KDF 参数），任何一处修改都要
立刻反映到其他 Tab 与状态栏。如果各 Tab 各自持有一份副本，必然出现
"题库改了这个 Tab 显示了、打包时用的还是旧数据"这类问题。

## 设计约束

- **校验规则只在 core/models.py 里**：本模块只负责"收集数据 → 交给 pydantic"，
  绝不重复实现规则。两份规则迟早会不一致。
- **不持有 master 密钥**：master 只在打包时由教师临时输入，用完即弃，
  绝不进入本对象、也不写进 project.json。
"""

from __future__ import annotations

import json
import secrets
from dataclasses import dataclass, field
from pathlib import Path

from pydantic import ValidationError

from app.core.models import ExamBank, KdfParams, Manifest, Session


@dataclass
class ProjectState:
    """一个考试项目的全部配置。"""

    #: exam_id -> 题库模型
    banks: dict[str, ExamBank] = field(default_factory=dict)
    #: 全局学号池（场次从这里挑人）
    students: list[int] = field(default_factory=list)
    #: 场次列表
    sessions: list[Session] = field(default_factory=list)
    #: Argon2id 参数
    kdf: KdfParams = field(default_factory=KdfParams)
    #: 全局 salt（hex）
    salt: str = ""
    #: 题库文件的原始路径（保存工程时记录，便于下次重新读取）
    bank_paths: dict[str, str] = field(default_factory=dict)

    def __post_init__(self) -> None:
        if not self.salt:
            # 16 字节随机 salt，与 Rust 端"至少 16 字节"的要求一致
            self.salt = secrets.token_hex(16)

    # ---------------------------------------------------------------- 题库

    def add_bank(self, bank: ExamBank, path: str | None = None) -> None:
        self.banks[bank.exam_id] = bank
        if path:
            self.bank_paths[bank.exam_id] = path

    def merge_banks(self, incoming: list[ExamBank], *, overwrite: bool) -> tuple[int, int, int]:
        """批量导入题库，返回 `(新增, 覆盖, 跳过)`。

        冲突 = exam_id 已存在（或同一批里出现两次）。
        旧实现直接 `banks[exam_id] = bank`，**静默替换**已有题库：
        场次还在引用同一个 exam_id，教师以为"加了一份"，
        实际把正在用的那门考试内容换掉了。
        """
        added = replaced = skipped = 0
        for bank in incoming:
            exists = bank.exam_id in self.banks
            if exists and not overwrite:
                skipped += 1
                continue
            if exists:
                replaced += 1
            else:
                added += 1
            self.banks[bank.exam_id] = bank
        return added, replaced, skipped

    def conflicting_bank_ids(self, incoming: list[ExamBank]) -> list[str]:
        """本批导入会与现有题库（或批内自身）冲突的 exam_id。"""
        seen: set[str] = set()
        conflicts: list[str] = []
        for bank in incoming:
            if bank.exam_id in self.banks or bank.exam_id in seen:
                if bank.exam_id not in conflicts:
                    conflicts.append(bank.exam_id)
            seen.add(bank.exam_id)
        return conflicts

    def remove_bank(self, exam_id: str) -> None:
        # 必须**先**摘场次引用，再删题库：
        # match_bank_exam_id 是拿 banks 做查找的，删掉之后所有引用都会
        # 解析成 None，反而认不出哪些引用指向被删的题库。
        for s in self.sessions:
            s.exams = [e for e in s.exams if match_bank_exam_id(e.file, self.banks) != exam_id]
        self.banks.pop(exam_id, None)
        self.bank_paths.pop(exam_id, None)

    # ---------------------------------------------------------------- 场次

    def upsert_session(self, session: Session) -> None:
        for i, s in enumerate(self.sessions):
            if s.session_id == session.session_id:
                self.sessions[i] = session
                return
        self.sessions.append(session)

    def remove_session(self, session_id: int) -> None:
        self.sessions = [s for s in self.sessions if s.session_id != session_id]

    def next_session_id(self) -> int:
        return max((s.session_id for s in self.sessions), default=0) + 1

    def new_draft_session(self) -> Session:
        """构造一个"草稿场次"：尚未选科目，时间默认从现在起两小时。

        草稿不做完整校验（否则教师在选科目之前就会一直报错），
        真正能否打包由 validate() 与 Rust 端决定。
        """
        import datetime as _dt

        from app.core.timeparse import local_to_rfc3339

        now = _dt.datetime.now().replace(minute=0, second=0, microsecond=0)
        start = now + _dt.timedelta(hours=1)
        # 若已有场次，新场次默认接在**最后一个场次之后**。
        # 否则连点两次「新建」会得到两个时间窗完全重叠的场次，
        # 立刻触发"同一学生同时在两场"的警告，教师会以为程序出错了。
        from app.core.timeparse import parse_rfc3339 as _parse

        latest_end = 0
        for x in self.sessions:
            try:
                latest_end = max(latest_end, _parse(x.end))
            except ValueError:
                continue
        if latest_end:
            import datetime as _d2

            last = _d2.datetime.fromtimestamp(latest_end)
            # 取整到小时，间隔 30 分钟
            start = last.replace(minute=0, second=0, microsecond=0) + _d2.timedelta(hours=1)
        end = start + _dt.timedelta(hours=2)
        return Session(
            session_id=self.next_session_id(),
            start=local_to_rfc3339(start.strftime("%Y-%m-%d %H:%M")),
            end=local_to_rfc3339(end.strftime("%Y-%m-%d %H:%M")),
            students=list(self.students),
            exams=[],
        )

    # ---------------------------------------------------------------- 校验

    def build_manifest(self) -> Manifest:
        """把当前状态组装成 Manifest 并做完整校验。

        这是**唯一的**"能否打包"判定入口：pydantic 的所有 model_validator
        都会在这里触发。UI 只需捕获 ValidationError 并把消息显示给教师。
        """
        return Manifest(
            version=2,
            salt=self.salt,
            sessions=self.sessions,
            kdf=self.kdf,
        )

    def validate(self) -> list[str]:
        """返回人类可读的问题列表；空列表表示可以打包。

        比直接抛 ValidationError 更适合 UI：教师想一次看到**所有**问题，
        而不是修一个再撞下一个。
        """
        problems: list[str] = []

        if not self.banks:
            problems.append("还没有导入任何题库")
        if not self.students:
            problems.append("还没有导入学生名单")
        if not self.sessions:
            problems.append("还没有编排任何场次")

        # 逐个场次单独校验，收集全部错误
        for s in self.sessions:
            try:
                Session.model_validate(s.model_dump())
            except ValidationError as e:
                for err in e.errors():
                    loc = ".".join(str(x) for x in err["loc"])
                    problems.append(f"场次 {s.session_id}: {loc}: {err['msg']}")

        # 场次必须至少有一门科目、至少一名学生（与 Rust 端一致）
        for s in self.sessions:
            if not s.exams:
                problems.append(f"场次 {s.session_id} 还没有选择任何考试科目")
            if not s.students:
                problems.append(f"场次 {s.session_id} 的学号名单为空")

        # 场次引用的题库必须存在（三层匹配，与 Rust 端一致）
        for s in self.sessions:
            for e in s.exams:
                if match_bank_exam_id(e.file, self.banks) is None:
                    problems.append(f"场次 {s.session_id} 引用了不存在的题库 {e.file}")

        # 同一场次不得重复引用同一题库（与 Rust 的 seen_exams 一致）。
        # 否则试卷里会出现两个同名科目，抽题数按 max 取值，学生拿到重复的卷子。
        for s in self.sessions:
            seen: dict[str, str] = {}
            for e in s.exams:
                key = match_bank_exam_id(e.file, self.banks) or e.file
                if key in seen:
                    problems.append(
                        f"场次 {s.session_id} 重复引用了题库 {e.file}（同一场次只能考一次）"
                    )
                else:
                    seen[key] = e.file

        # 同一学生跨重叠场次（与 Rust 端 check_student_overlap 对齐）
        problems.extend(self._check_student_overlap())

        # 整体 manifest 校验
        try:
            self.build_manifest()
        except ValidationError as e:
            for err in e.errors():
                loc = ".".join(str(x) for x in err["loc"])
                problems.append(f"清单: {loc}: {err['msg']}")

        return problems

    def _check_student_overlap(self) -> list[str]:
        """与 Rust 端 `check_student_overlap` 保持一致。

        Rust 端会拒绝"同一学生同时在两个重叠场次"，
        这里提前在 UI 里给出同样的判断，避免教师点打包才发现。
        """
        from app.core.timeparse import parse_rfc3339

        out: list[str] = []
        for i in range(len(self.sessions)):
            for j in range(i + 1, len(self.sessions)):
                a, b = self.sessions[i], self.sessions[j]
                try:
                    a_s, a_e = parse_rfc3339(a.start), parse_rfc3339(a.end)
                    b_s, b_e = parse_rfc3339(b.start), parse_rfc3339(b.end)
                except ValueError:
                    continue  # 时间格式问题由 Session 自身校验报
                if not (a_s < b_e and b_s < a_e):
                    continue  # 时间窗不重叠
                common = set(a.students) & set(b.students)
                if common:
                    sample = ", ".join(str(x) for x in sorted(common)[:5])
                    more = f" 等 {len(common)} 人" if len(common) > 5 else ""
                    out.append(
                        f"场次 {a.session_id} 与 {b.session_id} 时间重叠，"
                        f"且学生 {sample}{more} 同时在两场"
                    )
        return out

    # ---------------------------------------------------------------- 存取

    def save(self, path: Path) -> None:
        """保存工程配置。

        **绝不写入 master**：只存 salt、KDF、题库路径、学生、场次。
        题库内容也不存——教师会持续修改源文件，存路径更合理。
        """
        data = {
            "version": 1,
            "salt": self.salt,
            "kdf": self.kdf.model_dump(),
            "bank_paths": self.bank_paths,
            "students": self.students,
            "sessions": [s.model_dump() for s in self.sessions],
        }
        path.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")

    @classmethod
    def load(cls, path: Path) -> ProjectState:
        """加载工程配置，并重新读取题库源文件（校验其仍然合法）。"""
        data = json.loads(path.read_text(encoding="utf-8"))
        st = cls(
            kdf=KdfParams.model_validate(data.get("kdf", {})),
            salt=data["salt"],
            students=list(data.get("students", [])),
        )
        st.sessions = [Session.model_validate(s) for s in data.get("sessions", [])]
        st.bank_paths = dict(data.get("bank_paths", {}))

        # 重新读取题库：源文件可能已被教师修改
        for exam_id, p in st.bank_paths.items():
            try:
                text = Path(p).read_text(encoding="utf-8")
                st.banks[exam_id] = ExamBank.model_validate_json(text)
            except (OSError, UnicodeDecodeError, ValidationError):
                # 读不到或已失效：保留路径记录，由 UI 提示教师重新导入
                pass
        return st

    # ---------------------------------------------------------------- 统计

    def summary(self) -> str:
        return (
            f"{len(self.banks)} 个题库 · {len(self.students)} 名学生 · {len(self.sessions)} 个场次"
        )


def match_bank_exam_id(file: str, banks: dict[str, ExamBank]) -> str | None:
    """按 `file` 找到对应题库的 exam_id。

    必须与 Rust 端 `container::match_bank` 的**三层回退**完全一致，
    否则 UI 说"配置没问题"，打包时 Rust 却报"匹配不到题库"。
    顺序不能变：
      1. 文件名去 .json（如 `math.json` → `math`）
      2. 完整路径去 .json 并把 / 与 _ 换成 -（如 `demo/math.json` → `demo-math`）
      3. 第 1 层结果再把 _ 换成 -（如 `demo_math.json` → `demo-math`）
    """
    base = file.rsplit("/", 1)[-1]
    stem = base[: -len(".json")] if base.endswith(".json") else base
    full_key = (
        (file[: -len(".json")] if file.endswith(".json") else file)
        .replace("/", "-")
        .replace("_", "-")
    )

    for cand in (stem, full_key, stem.replace("_", "-")):
        if cand in banks:
            return cand
    return None
