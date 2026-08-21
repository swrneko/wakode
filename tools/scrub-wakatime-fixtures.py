#!/usr/bin/env python3
"""Заменить личные данные в снимке WakaTime на устойчивые заглушки.

Форма — вот что делает фикстуру фикстурой. Значения заменяются, ключи,
типы и структура сохраняются в неприкосновенности.

Замена детерминированная: заглушка — функция от пары (ключ, значение) и
только от неё. Ни порядок обхода, ни состав входного каталога на неё не
влияют, поэтому добавление одиннадцатого снимка не переписывает заглушки
в десяти прежних фикстурах. Иначе повторный прогон давал бы шумный диф, а
сверять обезличенное с предыдущей редакцией стало бы нечем.

Модуль не делает ничего при импорте: работа — в `main`, вызов — под
`if __name__ == "__main__"`. Это нужно тестам (`test_scrub_wakatime_fixtures.py`),
которые импортируют его функции.
"""
import hashlib
import json
import pathlib
import re
import sys

# Ключи, значения которых личные. Значение заменяется на заглушку того же
# типа: строка на строку, чтобы форма не поехала.
#
# Список дополнен по факту чтения снятых снимков (см. task-1-report.md):
# исходный список в спеке ловит по имени ключа, а не по содержимому, и
# пропускает личные данные под другими именами. Добавлено:
#   - "profile_url", "profile_url_escaped" — URL со встроенным id аккаунта;
#   - "last_branch", "last_project" — те же personal-поля ветки/проекта,
#     но с префиксом last_ у объекта пользователя (current.json);
#   - "last_plugin" — строка с точной версией ОС и сборки машины
#     ("linux-7.1.5-arch1-2-unknown"), фингерпринтит устройство;
#   - "user_agent_id" — id связки редактор+плагин+ОС, позволяет
#     сопоставлять heartbeats одному устройству;
#   - "ai_session" — id сессии ИИ-ассистента; в снятом снимке совпадает
#     с id текущей рабочей сессии агента, снимающего фикстуры;
#   - "github_username", "linkedin_username", "twitter_username",
#     "wonderfuldev_username" — в этом снимке все null и потому не
#     утекли, но это явно личные поля по смыслу ключа, и при повторном
#     снятии с другого аккаунта могут оказаться заполнены;
#   - "dependencies" — не имя и не путь, а содержимое файлов владельца
#     аккаунта: разобранные плагином импорты его исходников. В снимке
#     это ["re", "shutil", "subprocess"] в heartbeats-day.json. В
#     summaries-*.json под тем же ключом лежит массив объектов, у них
#     личное имя приезжает через "name", который в списке уже есть.
PERSONAL = {
    "email", "username", "full_name", "display_name", "website",
    "human_readable_website", "photo", "public_email", "city",
    "name", "project", "branch", "entity", "machine",
    "machine_name_id", "user_id", "id", "color",
    "profile_url", "profile_url_escaped", "last_branch", "last_project",
    "last_plugin", "user_agent_id", "ai_session",
    "github_username", "linkedin_username", "twitter_username",
    "wonderfuldev_username", "dependencies",
}

# Объекты, внутри которых имена ключей — это не поля с данными, а
# названия полей запроса, а значения — текст сообщения протокола.
#
# `errors` в ответе `heartbeats.bulk` устроен как {"entity": ["This field
# is required."]}. Ключ `entity` тут — имя отвергнутого поля, а не путь к
# файлу, и подстановка заглушки уничтожает ровно то сообщение, ради
# которого фикстура и снималась.
OPAQUE = {"errors"}

_UUID = re.compile(
    r"\A[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}"
    r"-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\Z"
)

# Позиции версии и варианта в каноническом написании UUID:
# xxxxxxxx-xxxx-Vxxx-Nxxx-xxxxxxxxxxxx
_VERSION_POS = 14
_VARIANT_POS = 19


def is_protocol_constant(value: str) -> bool:
    """UUID, у которого нулевые все разряды, кроме версии и варианта, —
    константа протокола, а не идентификатор.

    Про пользователя такое значение не говорит ничего, зато говорит о
    поведении сервера. WakaTime отвечает на отметку-дубликат телом
    {"id": "00000000-0000-4000-a000-000000000000", "skip": …}: id нулевой
    именно потому, что строки не появилось. Обезличив его, фикстура
    начинает утверждать, что там был обычный id, — и тест, написанный по
    такой фикстуре, проверял бы не то поведение.

    Правило узкое: у настоящего id живой отметки ненулевые разряды есть
    почти наверняка, и он по-прежнему заменяется.
    """
    if not _UUID.match(value):
        return False
    return all(
        char == "0"
        for position, char in enumerate(value)
        if char != "-" and position not in (_VERSION_POS, _VARIANT_POS)
    )


# Длина шестнадцатеричного хвоста заглушки: компромисс между читаемостью
# (`project-a3f19c` глазами разбирается, `project-9e3779b97f4a7c15` — нет)
# и вероятностью столкновения. В снятом снимке 889 различных личных
# значений, из них 504 под ключом `id`; на четырёх разрядах они дают
# четыре столкновения, на шести — с запасом ни одного (парадокс дней
# рождения: 504²/2/16⁶ ≈ 0.008). Столкновение не проходит молча — см.
# `placeholder`.
_TAIL = 6

# Занятые заглушки: заглушка → пара, которая её заняла. Нужна не для
# выдачи (та чистая функция от пары), а чтобы столкновение хвостов не
# слило два разных проекта в один молча.
_assigned: dict[str, tuple[str, str]] = {}


def placeholder(key: str, value: str) -> str:
    """Устойчивая заглушка для значения.

    Хвост — усечённый хеш от самой пары (ключ, значение), а не порядковый
    номер: номер зависел бы от того, в каком порядке встретились
    значения, и новый входной файл двигал бы заглушки во всех прежних
    фикстурах. `hash()` тут не годится — он рандомизирован между
    процессами (PYTHONHASHSEED), поэтому hashlib.
    """
    digest = hashlib.blake2b(
        f"{key}\0{value}".encode("utf-8"), digest_size=8
    ).hexdigest()
    slot = f"{key}-{digest[:_TAIL]}"
    taken = _assigned.setdefault(slot, (key, value))
    if taken != (key, value):
        raise RuntimeError(
            f"хвост заглушки {slot} столкнулся: {taken[1]!r} и {value!r}. "
            f"Два разных значения слились бы в одно — увеличьте _TAIL."
        )
    return slot


def scrub(node, key=None, opaque=False):
    if isinstance(node, dict):
        return {k: scrub(v, k, opaque or k in OPAQUE) for k, v in node.items()}
    if isinstance(node, list):
        return [scrub(v, key, opaque) for v in node]
    if isinstance(node, str) and key in PERSONAL and node and not opaque:
        if is_protocol_constant(node):
            return node
        return placeholder(key, node)
    return node


def main(argv: list[str]) -> None:
    src = pathlib.Path(argv[1] if len(argv) > 1 else "fixtures/wakatime")
    dst = pathlib.Path(argv[2] if len(argv) > 2 else
                       "crates/wakode-api/tests/fixtures/wakatime")
    dst.mkdir(parents=True, exist_ok=True)
    for path in sorted(src.glob("*.json")):
        data = json.loads(path.read_text())
        (dst / path.name).write_text(
            json.dumps(scrub(data), ensure_ascii=False, indent=2) + "\n"
        )
        print(f"  {path.name}")
    print(f"обезличено в {dst}")


if __name__ == "__main__":
    main(sys.argv)
