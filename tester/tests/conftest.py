from pathlib import Path

import pytest

from pgw_tester.config import Settings

from .fakecore import FakeCore


@pytest.fixture
def core() -> FakeCore:
    return FakeCore()


@pytest.fixture
def settings(tmp_path: Path) -> Settings:
    return Settings(
        admin_url="http://core-admin",
        data_url="http://core-data",
        mock_url="http://mock",
        mock_control_url="http://mock",
        data_dir=tmp_path,
    )
