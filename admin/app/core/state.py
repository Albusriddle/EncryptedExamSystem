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
import os
import secrets
import shutil
from dataclasses import dataclass, field
from pathlib import Path

from pydantic import ValidationError

from app.core.bank_import import BankImportError, load_bank_from_path, validate_bank_answers
from app.core.models import ExamBank, KdfParams, Manifest, Session, StudentClass


def _load_classes(raw: object, path: Path) -> list[StudentClass]:
    """解析班级列表，把 pydantic 的原始报错翻译成老师看得懂的一句话。"""
    from pydantic import ValidationError

    items = raw if isinstance(raw, list) else []
    out: list[StudentClass] = []
    for i, item in enumerate(items, start=1):
        try:
            out.append(StudentClass.model_validate(item))
        except ValidationError as e:
            first = e.errors()[0]
            msg = first.get("msg", "格式不正确")
            raise ValueError(
                f"工程文件里的第 {i} 个班级数据有问题：{msg}。\n"
                f"文件：{path.name}\n请检查该班级的学号或名字，或从备份恢复。"
            ) from e
    return out


#: 工程文件格式版本。
#:
#: 1 → 只有 students/sessions
#: 2 → 增加 classes（班级分组）
PROJECT_VERSION = 2


class ProjectLocked(Exception):
    """工程文件已加密，需要口令才能打开。"""


@dataclass
class ProjectState:
    """一个考试项目的全部配置。"""

    #: exam_id -> 题库模型
    banks: dict[str, ExamBank] = field(default_factory=dict)
    #: 全局学号池（场次从这里挑人）
    students: list[int] = field(default_factory=list)
    #: 班级：全局名单的命名分组（成员必须是 students 的子集）
    classes: list[StudentClass] = field(default_factory=list)
    #: 场次列表
    sessions: list[Session] = field(default_factory=list)
    #: Argon2id 参数
    kdf: KdfParams = field(default_factory=KdfParams)
    #: 全局 salt（hex）
    salt: str = ""
    #: 题库文件的原始路径（保存工程时记录，便于下次重新读取）
    bank_paths: dict[str, str] = field(default_factory=dict)
    #: 签名时间令牌（阶段 4.2）
    time_mode: str = "off"
    time_url: str = ""
    time_pubkey: str = ""
    time_max_skew: int = 300

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

    # ---------------------------------------------------------------- 班级

    def set_students(self, ids: list[int]) -> None:
        """替换全局名单，并把班级里已不存在的成员剪掉。

        为什么必须剪：班级成员是全局名单的子集是硬不变量；教师删人后
        班级里还留着旧学号，打包时就会出现"场次里有这个学生、名单里没有"。
        """
        self.students = list(dict.fromkeys(ids))
        alive = set(self.students)
        for c in self.classes:
            c.students = [sid for sid in c.students if sid in alive]

    def class_by_name(self, name: str) -> StudentClass | None:
        key = name.strip().casefold()
        return next((c for c in self.classes if c.key() == key), None)

    def add_class(self, name: str, code: str = "") -> StudentClass:
        if self.class_by_name(name) is not None:
            raise ValueError(f"班级名重复：{name}")
        c = StudentClass(name=name, code=code, students=[])
        self.classes.append(c)
        return c

    def remove_class(self, name: str) -> None:
        key = name.strip().casefold()
        self.classes = [c for c in self.classes if c.key() != key]

    def set_class_members(self, name: str, ids: list[int]) -> None:
        """设置班级成员；新学号自动并入全局名单。"""
        c = self.class_by_name(name)
        if c is None:
            raise ValueError(f"没有这个班级：{name}")
        uniq = list(dict.fromkeys(ids))
        c.students = uniq
        for sid in uniq:
            if sid not in self.students:
                self.students.append(sid)

    def classes_of(self, student_id: int) -> list[str]:
        return [c.name for c in self.classes if student_id in c.students]

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
        from app.core.models import TimeConfig

        return Manifest(
            version=2,
            salt=self.salt,
            sessions=self.sessions,
            kdf=self.kdf,
            time=TimeConfig(
                mode=self.time_mode,
                url=self.time_url,
                pubkey=self.time_pubkey,
                max_skew=self.time_max_skew,
            ),
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

        # 班级：名字唯一、成员必须在全局名单里
        seen_names: set[str] = set()
        for c in self.classes:
            if c.key() in seen_names:
                problems.append(f"班级名重复：{c.name}")
            seen_names.add(c.key())
            missing = [sid for sid in c.students if sid not in self.students]
            if missing:
                problems.append(f"班级 {c.name} 的成员不在学生名单里：{missing[:5]}")

        # 答案自洽性：原生题库以前只查形状，`answer: "E"` 配 A/B 选项也能过。
        # 放在这里而不是只在导入时查，是因为题库源文件可能在导入后被改过。
        for bank in self.banks.values():
            problems.extend(validate_bank_answers(bank))

        # 时间令牌配置：提前给出可读错误，而不是让 build_manifest 抛 pydantic 异常
        from app.core.models import TimeConfig

        try:
            TimeConfig(
                mode=self.time_mode,
                url=self.time_url,
                pubkey=self.time_pubkey,
                max_skew=self.time_max_skew,
            )
        except Exception as e:  # noqa: BLE001
            problems.append(f"时间令牌配置：{e}")

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
            if not s.exams and not s.sections:
                problems.append(f"场次 {s.session_id} 还没有选择任何考试科目/大题")
            if not s.students:
                problems.append(f"场次 {s.session_id} 的学号名单为空")

        # 难度区间：与保存/打包共用同一套校验，避免"检查配置通过、打包失败"
        from app.core.difficulty import check_difficulty_range

        for s in self.sessions:
            for e in s.exams:
                if e.difficulty_min is None:
                    continue
                key = match_bank_exam_id(e.file, self.banks)
                bank = self.banks.get(key) if key else None
                if bank is None:
                    continue
                problems.extend(
                    f"场次 {s.session_id}: {msg}"
                    for msg in check_difficulty_range(
                        bank,
                        e.pick or bank.pick,
                        e.difficulty_min,
                        e.difficulty_max or e.difficulty_min,
                    )
                )

        # 场次引用的题库必须存在（三层匹配，与 Rust 端一致）
        for s in self.sessions:
            for e in s.exams:
                if match_bank_exam_id(e.file, self.banks) is None:
                    problems.append(f"场次 {s.session_id} 引用了不存在的题库 {e.file}")

        # 同一场次不得重复引用同一题库（旧“按题库”模式；大题模式允许
        # 同一题库拆成多个大题，因此只在没有 sections 时检查）。
        for s in self.sessions:
            if s.sections:
                continue
            seen: dict[str, str] = {}
            for e in s.exams:
                key = match_bank_exam_id(e.file, self.banks) or e.file
                if key in seen:
                    problems.append(
                        f"场次 {s.session_id} 重复引用了题库 {e.file}（同一场次只能考一次）"
                    )
                else:
                    seen[key] = e.file

        # 大题模式：编号唯一、题库存在、题数不超过该难度可用题数
        for s in self.sessions:
            seen_sections: set[str] = set()
            seen_titles: set[str] = set()
            per_bank: dict[str, int] = {}
            per_diff: dict[tuple[str, int], int] = {}
            for sec in s.sections:
                if sec.section_id in seen_sections:
                    problems.append(f"场次 {s.session_id}: 大题编号 {sec.section_id} 重复")
                seen_sections.add(sec.section_id)
                if sec.title in seen_titles:
                    problems.append(f"场次 {s.session_id}: 大题标题 {sec.title} 重复")
                seen_titles.add(sec.title)
                key = match_bank_exam_id(sec.file, self.banks)
                if key is None:
                    problems.append(
                        f"场次 {s.session_id}: 大题 {sec.title} 引用了不存在的题库 {sec.file}"
                    )
                    continue
                bank = self.banks[key]
                parts = (
                    [(sec.title, sec.count, sec.difficulty)]
                    if not sec.parts
                    else [(p.title, p.count, p.difficulty) for p in sec.parts]
                )
                part_ids: set[str] = set()
                for part in sec.parts:
                    if part.part_id in part_ids:
                        problems.append(f"场次 {s.session_id}: 部分编号 {part.part_id} 重复")
                    part_ids.add(part.part_id)
                part_titles: set[str] = set()
                for ptitle, pcount, pdiff in parts:
                    if ptitle in part_titles:
                        problems.append(
                            f"场次 {s.session_id}: 大题 {sec.title} 的部分标题 {ptitle} 重复"
                        )
                    part_titles.add(ptitle)
                    available = sum(1 for q in bank.questions if q.difficulty == pdiff)
                    if pcount > available:
                        problems.append(
                            f"场次 {s.session_id}: 部分 {sec.title}/{ptitle} 要抽 "
                            f"{pcount} 题，但题库 {bank.title} 难度 {pdiff} 只有 "
                            f"{available} 题"
                        )
                    per_bank[key] = per_bank.get(key, 0) + pcount
                    per_diff[(key, pdiff)] = per_diff.get((key, pdiff), 0) + pcount
            for key, need in per_bank.items():
                bank = self.banks[key]
                if need > len(bank.questions):
                    problems.append(
                        f"场次 {s.session_id}: 题库 {bank.title} 多个大题共需 {need} 题，"
                        f"但只有 {len(bank.questions)} 题"
                    )
            for (key, diff), need in per_diff.items():
                bank = self.banks[key]
                available = sum(1 for q in bank.questions if q.difficulty == diff)
                if need > available:
                    problems.append(
                        f"场次 {s.session_id}: 题库 {bank.title} 难度 {diff} 多个大题"
                        f"共需 {need} 题，但只有 {available} 题"
                    )

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

    def save(self, path: Path, passphrase: str | None = None) -> bool:
        """保存工程配置；passphrase 非空时写加密格式（EESPROJ3）。

        返回 True 表示请求了加密但本机缺少加密依赖，已降级为明文保存。
        绝不写入 master：只存 salt、KDF、题库路径、学生、场次。
        题库内容也不存——教师会持续修改源文件，存路径更合理。
        """
        data = {
            "version": PROJECT_VERSION,
            "salt": self.salt,
            "kdf": self.kdf.model_dump(),
            "bank_paths": self.bank_paths,
            "time": {
                "mode": self.time_mode,
                "url": self.time_url,
                "pubkey": self.time_pubkey,
                "max_skew": self.time_max_skew,
            },
            "students": self.students,
            "classes": [c.model_dump() for c in self.classes],
            "sessions": [s.model_dump() for s in self.sessions],
        }
        raw = json.dumps(data, ensure_ascii=False, indent=2).encode("utf-8")
        downgraded = False
        if passphrase:
            try:
                from app.core import secure

                raw = secure.encrypt_project(raw, passphrase)
            except ModuleNotFoundError:
                downgraded = True
        tmp = path.with_name(path.name + ".tmp")
        try:
            tmp.write_bytes(raw)
            if path.exists():
                shutil.copy2(path, path.with_name(path.name + ".bak"))
            os.replace(tmp, path)
        finally:
            tmp.unlink(missing_ok=True)
        return downgraded

    @classmethod
    def load(cls, path: Path, passphrase: str | None = None) -> ProjectState:
        """加载工程配置，并重新读取题库源文件（校验其仍然合法）。"""
        from app.core import secure

        raw = path.read_bytes()
        if secure.is_encrypted_project(raw):
            if not passphrase:
                raise ProjectLocked("工程已加密，需要口令才能打开。")
            raw = secure.decrypt_project(raw, passphrase)
        data = json.loads(raw.decode("utf-8"))
        salt = data.get("salt")
        if not salt:
            raise ValueError(f"{path.name} 缺少 salt 字段，可能不是本程序保存的工程文件。")
        version = int(data.get("version", 1))
        if version > PROJECT_VERSION:
            raise ValueError(
                f"工程文件版本 {version} 比本程序支持的 {PROJECT_VERSION} 新，"
                "请升级管理端后再打开。"
            )
        st = cls(
            kdf=KdfParams.model_validate(data.get("kdf", {})),
            salt=salt,
            students=list(data.get("students", [])),
            classes=_load_classes(data.get("classes", []), path),
        )
        st.sessions = [Session.model_validate(s) for s in data.get("sessions", [])]
        st.bank_paths = dict(data.get("bank_paths", {}))
        t = data.get("time") or {}
        st.time_mode = str(t.get("mode") or "off")
        st.time_url = str(t.get("url") or "")
        st.time_pubkey = str(t.get("pubkey") or "")
        st.time_max_skew = int(t.get("max_skew") or 300)

        # 重新读取题库：源文件可能已被教师修改。
        #
        # 必须走 `load_bank_from_path` 而不是直接 `ExamBank.model_validate_json`：
        # 外部题库的源文件是 `X.questions.json`（题目/答案分离的格式），
        # 按原生形状解析必然失败，题库会被静默丢弃——实测重开工程后
        # "题库全没了"。这里由适配器负责识别格式并合并答案。
        for exam_id, p in st.bank_paths.items():
            try:
                bank = load_bank_from_path(Path(p))
            except (OSError, UnicodeDecodeError, ValidationError, BankImportError, ValueError):
                # 读不到或已失效：保留路径记录，由 UI 提示教师重新导入
                continue
            # 以工程里记录的 exam_id 为准（教师可能在导入后改过名）
            bank.exam_id = exam_id
            st.banks[exam_id] = bank
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
