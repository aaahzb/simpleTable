"""FilterProxy 单元测试（离屏 Qt）。"""
import pytest

from filter_proxy import FilterProxy
from table_model import TableModel


@pytest.fixture()
def proxy(qapp) -> FilterProxy:
    """三行两列模型 + 筛选代理。"""
    m = TableModel()
    m.load(["目的地", "备注"],
           [["大理古城", "古城\n漫步"],
            ["黄山", "登山 看云海"],
            ["大理洱海", "骑行"]])
    p = FilterProxy()
    p.setSourceModel(m)
    return p


def _shown_names(p: FilterProxy) -> list[str]:
    """读取当前可见行的目的地。

    Args:
        p: 筛选代理。

    Returns:
        list[str]: 可见行第 0 列文本。
    """
    return [p.index(r, 0).data() for r in range(p.rowCount())]


class TestFilter:
    def test_substring(self, proxy) -> None:
        proxy.set_filter(0, "大理")
        assert _shown_names(proxy) == ["大理古城", "大理洱海"]

    def test_case_insensitive_latin(self, qapp) -> None:
        m = TableModel()
        m.load(["X"], [["AbC"], ["def"]])
        p = FilterProxy()
        p.setSourceModel(m)
        p.set_filter(0, "abc")
        assert p.rowCount() == 1

    def test_multi_column_and(self, proxy) -> None:
        proxy.set_filter(0, "大理")
        proxy.set_filter(1, "骑行")
        assert _shown_names(proxy) == ["大理洱海"]

    def test_filter_matches_multiline_full_content(self, proxy) -> None:
        # "漫步" 在多行内容里，筛选须按完整内容匹配而非格内显示文本
        proxy.set_filter(1, "漫步")
        assert _shown_names(proxy) == ["大理古城"]

    def test_empty_text_clears_column(self, proxy) -> None:
        proxy.set_filter(0, "大理")
        proxy.set_filter(0, "")
        assert proxy.rowCount() == 3

    def test_clear_filters(self, proxy) -> None:
        proxy.set_filter(0, "大理")
        proxy.clear_filters()
        assert proxy.rowCount() == 3
        assert proxy.active_count() == 0
