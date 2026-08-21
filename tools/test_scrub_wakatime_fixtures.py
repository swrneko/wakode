#!/usr/bin/env python3
"""Тесты обезличивателя снимков WakaTime.

Запуск: `python3 -m unittest discover -s tools`.

Механизмы обезличивателя раньше держались одними комментариями, и один
из них (константа протокола в поле `id`) от этого сломался незамеченным.
Здесь проверяется поведение через публичные функции модуля.
"""
import importlib.util
import pathlib
import unittest

# Имя файла с дефисами — обычным `import` не берётся.
_PATH = pathlib.Path(__file__).with_name("scrub-wakatime-fixtures.py")
_SPEC = importlib.util.spec_from_file_location("scrub_wakatime_fixtures", _PATH)
scrubber = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(scrubber)


class ScrubTest(unittest.TestCase):
    def test_a_message_inside_errors_survives_scrubbing(self):
        # Форма ответа heartbeats.bulk на отвергнутый элемент: ключ
        # `entity` тут — имя поля запроса, значение — проза протокола.
        bulk = {"errors": {"entity": ["This field is required."]}}
        self.assertEqual(scrubber.scrub(bulk), bulk)

    def test_a_protocol_constant_uuid_survives_but_a_real_one_does_not(self):
        constant = "00000000-0000-4000-a000-000000000000"
        real = "0198c3f1-7a2b-7c4d-8e5f-a1b2c3d4e5f6"
        scrubbed = scrubber.scrub({"id": constant, "user_id": real})
        self.assertEqual(scrubbed["id"], constant)
        self.assertNotEqual(scrubbed["user_id"], real)
        self.assertTrue(scrubbed["user_id"].startswith("user_id-"))

    def test_a_uuid_with_one_nonzero_digit_is_not_a_constant(self):
        # Граница правила: константой считается только полностью нулевой
        # UUID. Один ненулевой разряд — уже идентификатор.
        self.assertFalse(
            scrubber.is_protocol_constant("00000000-0000-4000-a000-000000000001")
        )
        self.assertTrue(
            scrubber.is_protocol_constant("00000000-0000-4000-a000-000000000000")
        )

    def test_equal_values_share_a_placeholder_and_different_ones_do_not(self):
        scrubbed = scrubber.scrub(
            {"data": [{"project": "alpha"}, {"project": "alpha"}, {"project": "beta"}]}
        )
        first, second, third = (item["project"] for item in scrubbed["data"])
        self.assertEqual(first, second)
        self.assertNotEqual(first, third)

    def test_adding_a_new_value_leaves_earlier_placeholders_in_place(self):
        before = scrubber.scrub({"project": "alpha", "branch": "main"})
        after = scrubber.scrub(
            {"aaa": {"project": "zulu"}, "project": "alpha", "branch": "main"}
        )
        self.assertEqual(before["project"], after["project"])
        self.assertEqual(before["branch"], after["branch"])

    def test_a_placeholder_collision_is_loud_rather_than_silent(self):
        # Заглушки обязаны быть различимы: столкнись хвост — два разных
        # проекта слились бы в один, и фикстура перестала бы показывать,
        # что их было два. Хвост укорачивается до одного разряда, чтобы
        # столкновение стало неизбежным по принципу Дирихле.
        tail = scrubber._TAIL
        scrubber._TAIL = 1
        try:
            with self.assertRaises(RuntimeError):
                for number in range(64):
                    scrubber.placeholder("project", f"проект-{number}")
        finally:
            scrubber._TAIL = tail

    def test_a_key_outside_personal_is_left_alone(self):
        untouched = {"category": "coding", "language": "Rust", "skip": "Too many"}
        self.assertEqual(scrubber.scrub(untouched), untouched)

    def test_a_dependencies_array_stays_an_array_of_the_same_length(self):
        scrubbed = scrubber.scrub({"dependencies": ["re", "shutil", "subprocess"]})
        self.assertIsInstance(scrubbed["dependencies"], list)
        self.assertEqual(len(scrubbed["dependencies"]), 3)
        self.assertNotIn("subprocess", scrubbed["dependencies"])

    def test_dependencies_of_summaries_stay_objects_with_the_same_keys(self):
        # В summaries под тем же ключом лежит массив объектов, а не строк:
        # заглушка обязана уехать в `name`, а не подменить объект строкой.
        item = {"name": "shutil", "total_seconds": 51.658, "percent": 33.33}
        scrubbed = scrubber.scrub({"dependencies": [item]})["dependencies"][0]
        self.assertEqual(list(scrubbed), list(item))
        self.assertNotEqual(scrubbed["name"], item["name"])
        self.assertEqual(scrubbed["total_seconds"], item["total_seconds"])


if __name__ == "__main__":
    unittest.main()
