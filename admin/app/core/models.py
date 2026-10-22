"""数据模型与校验。

字段与 Rust 端 crates/common/src/model.rs 严格对应——两端不一致会导致
打包出来的包解不开，所以这里的字段名/类型改动必须同步改 Rust 侧。
"""

from __future__ import annotations

from pydantic import BaseModel, Field, field_validator, model_validator

# u64 上限。不限制 INT_MAX：真实学号常为 12 位，早已超出 i32。
U64_MAX = 2**64 - 1


class Question(BaseModel):
    id: str
    qtype: str = Field(alias="type")
    stem: str
    options: list[str] = Field(default_factory=list)
    answer: str | None = None
    score: int = 2

    model_config = {"populate_by_name": True}


class ExamBank(BaseModel):
    exam_id: str
    title: str
    pick: int
    questions: list[Question]

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
    pick: int | None = None


class Session(BaseModel):
    session_id: int
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
        if self.start >= self.end:
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
        return self


class Manifest(BaseModel):
    version: int = 2
    salt: str
    sessions: list[Session]
    time_sources: list[str] = Field(default_factory=list)
    time_pubkey: str = ""
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
