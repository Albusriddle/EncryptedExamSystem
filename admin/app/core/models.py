"""数据模型与校验。

字段与 Rust 端 crates/common/src/model.rs 严格对应——两端不一致会导致
打包出来的包解不开，所以这里的字段名/类型改动必须同步改 Rust 侧。
"""

from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, Field, field_validator, model_validator

from app.core.timeparse import parse_rfc3339

# u64 上限。不限制 INT_MAX：真实学号常为 12 位，早已超出 i32。
U64_MAX = 2**64 - 1


class ChoiceOption(BaseModel):
    """带键的选项。键（A/B/C…）是答案的坐标，必须原样保留。"""

    key: str
    text: str


class QuestionBody(BaseModel):
    """题目正文形态，与 Rust 的 `QuestionBody` 一一对应（serde tag = kind）。

    为什么要分形态：补全题是带占位符的代码模板、改错题是逐行代码，
    它们既不是选项、也不该压成一段纯文本（压了就没法渲染代码块、
    没法定位"第几行"）。`text` 是旧题库的默认值。
    """

    kind: Literal["text", "options", "template", "code"] = "text"
    options: list[ChoiceOption] = Field(default_factory=list)
    template: str | None = None
    code: list[str] = Field(default_factory=list)
    # 与 Rust 的 u8 对齐：超范围的值 Python 放行、打包时才报"解析题库"
    blank_count: int = Field(default=0, ge=0, le=255)

    @model_validator(mode="after")
    def _check(self) -> QuestionBody:
        if self.kind == "options":
            if not self.options:
                raise ValueError("kind=options 时 options 不能为空")
            # 答题卡用字母拼接表示多选答案（"AC"），超过 26 个会出现 "27"
            # 这种多字符键，和 "2"+"7" 无法区分——在入口就拦住。
            if len(self.options) > 26:
                raise ValueError("选项不能超过 26 个（A–Z）")
            keys = [o.key for o in self.options]
            if len(keys) != len(set(keys)):
                raise ValueError("选项键重复")
        elif self.kind == "template":
            if not (self.template or "").strip():
                raise ValueError("kind=template 时 template 不能为空")
            if self.blank_count < 1:
                raise ValueError("kind=template 时 blank_count 必须 ≥ 1")
        elif self.kind == "code":
            if not self.code:
                raise ValueError("kind=code 时 code 不能为空")
            if self.blank_count < 1:
                raise ValueError("kind=code 时 blank_count 必须 ≥ 1")
        return self


class Question(BaseModel):
    id: str
    qtype: str = Field(alias="type")
    stem: str
    options: list[str] = Field(default_factory=list)
    body: QuestionBody = Field(default_factory=QuestionBody)
    kp: str | None = None
    difficulty: int | None = Field(default=None, ge=0, le=255)
    #: 结构化答案（single/multi/blanks/fixes）。打包时会被剥离。
    answer: Any = None
    #: 判分材料（entry/harness/tests）。**绝不进包**，打包时会被剥离。
    judge: Any = None
    # Rust 是 u32；这里更严（0..=1000），因为"分值上万"没有实际意义
    score: int = Field(default=2, ge=0, le=1000)

    model_config = {"populate_by_name": True}


#: Windows 保留设备名：即使带了扩展名也不能用作文件名。
_WIN_RESERVED = {
    "con",
    "prn",
    "aux",
    "nul",
    *(f"com{i}" for i in range(1, 10)),
    *(f"lpt{i}" for i in range(1, 10)),
}

#: 文件名里绝对不能出现的字符（路径分隔符 / Windows 禁用字符 / 流分隔符）。
_FORBIDDEN_CHARS = set('/\\:*?"<>|')


def validate_exam_id(v: str) -> str:
    """校验 exam_id 可以安全地当作文件名主干。

    `exam_id` 会参与 `tmpdir / f"{exam_id}.json"`，所以必须挡住路径成分。
    旧实现只校验"非空"，实测 `exam_id="../../escaped"` 可以把 JSON
    写到临时目录之外（导入一个恶意题库就能覆盖任意可写目录下的 .json）。

    **按危险特征拒绝**而不是限定 ASCII 白名单：中文 exam_id（如「数学-2」）
    是合理用法，不该被误伤；真正危险的是分隔符、控制字符、保留设备名
    以及 Windows 会自动修剪的首尾点/空格。
    """
    if not v:
        raise ValueError("exam_id 不能为空")
    if len(v) > 64:
        raise ValueError(f"exam_id 过长（{len(v)} 字符，最多 64）")
    if any(c in _FORBIDDEN_CHARS for c in v):
        raise ValueError(f"exam_id 不能包含路径分隔符或 Windows 禁用字符（当前 {v!r}）")
    if any(ord(c) < 0x20 or c.isspace() for c in v):
        raise ValueError(f"exam_id 不能包含空白或控制字符（当前 {v!r}）")
    if v.startswith(".") or v.endswith("."):
        raise ValueError(f"exam_id 不能以 . 开头或结尾（当前 {v!r}）")
    if v.lower() in _WIN_RESERVED:
        raise ValueError(f"exam_id 不能使用 Windows 保留名（当前 {v!r}）")
    return v


class ExamBank(BaseModel):
    exam_id: str
    title: str
    # 下限保持 0（空卷是允许的，用户明确要求）；上限对齐 Rust 的 u32
    pick: int = Field(ge=0, le=2**32 - 1)
    questions: list[Question]

    @field_validator("exam_id")
    @classmethod
    def _check_exam_id(cls, v: str) -> str:
        return validate_exam_id(v)

    @model_validator(mode="after")
    def _check(self) -> ExamBank:
        if not self.exam_id:
            raise ValueError("exam_id 不能为空")
        if self.pick > len(self.questions):
            raise ValueError(
                f"题库 {self.exam_id} 抽题数 {self.pick} 超过题量 {len(self.questions)}"
            )
        ids = [q.id for q in self.questions]
        if len(ids) != len(set(ids)):
            raise ValueError(f"题库 {self.exam_id} 存在重复题目 ID")
        return self


class SessionExam(BaseModel):
    file: str
    # 上限对齐 Rust 的 u32；下限为 0（空卷允许）
    pick: int | None = Field(default=None, ge=0, le=2**32 - 1)


class Session(BaseModel):
    # Rust 是 u32
    session_id: int = Field(ge=0, le=2**32 - 1)
    start: str
    end: str
    students: list[int]
    exams: list[SessionExam]

    @field_validator("students")
    @classmethod
    def _check_students(cls, v: list[int]) -> list[int]:
        """学号必须是正整数。

        **不限制 INT_MAX**：真实学号常为 12 位（如 202312345678），
        早已超出 i32。内部与 Rust 端一致使用 u64，上限 1.8e19 足够任何场景。
        与 `crates/common/src/container.rs::validate_sessions` 保持一致。
        """
        for sid in v:
            if sid <= 0:
                raise ValueError(f"学号必须是正整数: {sid}")
            if sid > U64_MAX:
                raise ValueError(f"学号超出 u64 范围: {sid}")
        if len(v) != len(set(v)):
            raise ValueError("学号列表存在重复")
        return v

    @model_validator(mode="after")
    def _check_time(self) -> Session:
        """校验时间窗合法。

        **必须解析成时间戳再比较**，不能直接比字符串：
        字符串比较只在"同一时区偏移、同一格式"时才等价于时间比较。
        实测 `2026-10-23T03:00:00Z` 与 `2026-10-23T10:00:00+08:00`
        （实际是 02:00Z，即倒挂）按字符串会被判为合法，
        直到打包时 Rust 端才发现，教师白填一遍表单。
        """
        try:
            start_ts = parse_rfc3339(self.start)
        except ValueError as exc:
            raise ValueError(
                f"场次 {self.session_id} 的开始时间不是 RFC3339: {self.start!r}"
            ) from exc
        try:
            end_ts = parse_rfc3339(self.end)
        except ValueError as exc:
            raise ValueError(
                f"场次 {self.session_id} 的结束时间不是 RFC3339: {self.end!r}"
            ) from exc
        import re

        # 必须显式带时区。不带偏移的 "2026-10-23 11:00" 会被当成 **UTC**，
        # 教师若按北京时间手写就会整体错 8 小时，而且两端解析结果一致、
        # 不报任何错——属于"静默偏移"，比报错更难查。
        tz = re.compile(r"(Z|[+-]\d{2}:?\d{2})$")
        for label, val in (("开始", self.start), ("结束", self.end)):
            if not tz.search(val.strip()):
                raise ValueError(
                    f"场次 {self.session_id} 的{label}时间必须带时区偏移"
                    f"（如 2026-10-23T11:00:00+08:00），当前 {val!r}。"
                    "不带偏移会被当作 UTC，导致时间整体偏移。"
                )
        if start_ts >= end_ts:
            raise ValueError(
                f"场次 {self.session_id} 的开始时间必须早于结束时间"
                f"（当前 {self.start} >= {self.end}）"
            )
        # 注意：这里**不校验** exams 是否为空。
        # 教师在界面上点「新建」时本来就还没选科目，此时构造 Session
        # 会抛 ValidationError，导致 Qt 槽函数静默失败（表现为"点了没反应"）。
        # "必须有科目"由 ProjectState.validate() 提示、由 Rust 端打包时强制。
        return self


class KdfParams(BaseModel):
    """Argon2id 参数。与 Rust 端 crypto::KdfParams 对应。

    这些值同时决定"打包耗时"与"攻击者枚举名单的成本"：
    - 默认 m=65536(64MiB) / t=3 / p=1，单次约 92ms（本机实测）；
    - 1000 名学生的包约 12 秒（8 线程）；
    - 攻击者 8 线程枚举 10^5 个学号约 19 分钟。
    **不要为了"打包快"把这些值调低。** 旧默认 19MiB/t=2 实测只需 3.8 分钟即可枚举完。
    """

    m_cost: int = 65536
    t_cost: int = 3
    p_cost: int = 1

    @model_validator(mode="after")
    def _check(self) -> KdfParams:
        if self.m_cost < 65536:
            raise ValueError(f"m_cost 过低（{self.m_cost}），会让学号枚举变得廉价")
        if self.t_cost < 3:
            raise ValueError(f"t_cost 过低（{self.t_cost}）")
        if self.p_cost < 1:
            raise ValueError("p_cost 必须 ≥ 1")
        # 上限：与 Rust 侧 KdfParams::validate 保持一致。
        # 没有上限时，一个手改的 project.json 能让打包进程直接吃光内存。
        if self.m_cost > 1_048_576:
            raise ValueError(f"m_cost 过高（{self.m_cost}），最多 1048576（1 GiB）")
        if self.t_cost > 16:
            raise ValueError(f"t_cost 过高（{self.t_cost}），最多 16")
        if self.p_cost > 8:
            raise ValueError(f"p_cost 过高（{self.p_cost}），最多 8")
        return self


class Manifest(BaseModel):
    version: int = 2
    salt: str
    sessions: list[Session]
    # 注：曾有 time_sources / time_pubkey 两字段，但无任何代码使用，
    # 属于"配置了不生效"的死配置，已删除（详见 model.rs 同处注释）。
    kdf: KdfParams = Field(default_factory=KdfParams)

    @field_validator("salt")
    @classmethod
    def _check_salt(cls, v: str) -> str:
        try:
            raw = bytes.fromhex(v)
        except ValueError as exc:
            raise ValueError("salt 必须是十六进制字符串") from exc
        if len(raw) < 16:
            raise ValueError("salt 至少 16 字节（32 个 hex 字符）")
        return v

    @model_validator(mode="after")
    def _check(self) -> Manifest:
        if not self.sessions:
            raise ValueError("清单里至少要有一个场次")
        ids = [s.session_id for s in self.sessions]
        if len(ids) != len(set(ids)):
            raise ValueError("场次编号重复")
        return self
