from pathlib import Path

from app.core.state import ProjectState


def test_student_review_survives_save_and_load(tmp_path: Path) -> None:
    state = ProjectState()
    state.student_review = "free"
    path = tmp_path / "review.eesproj"
    state.save(path)
    restored = ProjectState.load(path)
    assert restored.student_review == "free"
