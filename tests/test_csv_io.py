"""csv_io 模块单元测试。"""
import csv

from csv_io import read_csv, write_csv


class TestRoundTrip:
    """读写回环测试。"""

    def test_multiline_tab_quote_comma(self, tmp_path) -> None:
        path = str(tmp_path / "t.csv")
        headers = ["目的地", "备注"]
        rows = [["大理", "古城\n洱海\t骑行，含逗号 \"引号\""], ["丽江", "雪山"]]
        write_csv(path, headers, rows)
        got_headers, got_rows, warnings = read_csv(path)
        assert got_headers == headers
        assert got_rows == rows
        assert warnings == []

    def test_written_file_is_utf8_sig(self, tmp_path) -> None:
        path = str(tmp_path / "t.csv")
        write_csv(path, ["a"], [["1"]])
        with open(path, "rb") as f:
            assert f.read(3) == b"\xef\xbb\xbf"

    def test_read_gbk(self, tmp_path) -> None:
        path = str(tmp_path / "gbk.csv")
        with open(path, "w", encoding="gbk", newline="") as f:
            writer = csv.writer(f)
            writer.writerow(["目的地", "最佳季节"])
            writer.writerow(["大理", "春季"])
        headers, rows, _ = read_csv(path)
        assert headers == ["目的地", "最佳季节"]
        assert rows[0][0] == "大理"

    def test_ragged_rows(self, tmp_path) -> None:
        path = str(tmp_path / "rag.csv")
        with open(path, "w", encoding="utf-8-sig", newline="") as f:
            writer = csv.writer(f)
            writer.writerow(["a", "b"])
            writer.writerow(["1"])            # 短行
            writer.writerow(["1", "2", "3"])  # 长行
        _, rows, warnings = read_csv(path)
        assert rows == [["1", ""], ["1", "2"]]
        assert len(warnings) == 2

    def test_empty_file(self, tmp_path) -> None:
        path = str(tmp_path / "empty.csv")
        open(path, "w").close()
        assert read_csv(path) == ([], [], [])
