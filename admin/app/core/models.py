"""数据模型与校验。

字段与 Rust 端 crates/common/src/model.rs 严格对应——两端不一致会导致
打包出来的包解不开，所以这里的字段名/类型改动必须同步改 Rust 侧。
"""

from __future__ import annotations

from pydantic import BaseModel, Field, field_validator, model_validator

INT_MAX = 2_147_483_647


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
        for sid in v:
            if sid < 0:
                raise ValueError(f"学号不能为负: {sid}")
            if sid > INT_MAX:
                raise ValueError(f"学号 {sid} 超过 INT_MAX（{INT_MAX}）")
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
        if not self.exams:
            raise ValueError(f"场次 {self.session_id} 没有任何考试科目")
        return self


class Manifest(BaseModel):
    version: int = 1
    salt: str
    sessions: list[Session]
    time_sources: list[str] = Field(default_factory=list)
    time_pubkey: str = ""

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
