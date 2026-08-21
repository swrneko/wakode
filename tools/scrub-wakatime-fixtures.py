#!/usr/bin/env python3
"""Заменить личные данные в снимке WakaTime на устойчивые заглушки.

Форма — вот что делает фикстуру фикстурой. Значения заменяются, ключи,
типы и структура сохраняются в неприкосновенности.

Замена детерминированная: одно и то же исходное значение всегда даёт одну
и ту же заглушку. Иначе повторный прогон давал бы шумный диф, а сверять
обезличенное с предыдущей редакцией стало бы нечем.
"""
import json
import pathlib
import sys

SRC = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "fixtures/wakatime")
DST = pathlib.Path(sys.argv[2] if len(sys.argv) > 2 else
                   "crates/wakode-api/tests/fixtures/wakatime")

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
#     снятии с другого аккаунта могут оказаться заполнены.
PERSONAL = {
    "email", "username", "full_name", "display_name", "website",
    "human_readable_website", "photo", "public_email", "city",
    "name", "project", "branch", "entity", "machine",
    "machine_name_id", "user_id", "id", "color",
    "profile_url", "profile_url_escaped", "last_branch", "last_project",
    "last_plugin", "user_agent_id", "ai_session",
    "github_username", "linkedin_username", "twitter_username",
    "wonderfuldev_username",
}

# Объекты, внутри которых имена ключей — это не поля с данными, а
# названия полей запроса, а значения — текст сообщения протокола.
#
# `errors` в ответе `heartbeats.bulk` устроен как {"entity": ["This field
# is required."]}. Ключ `entity` тут — имя отвергнутого поля, а не путь к
# файлу, и подстановка заглушки уничтожает ровно то сообщение, ради
# которого фикстура и снималась.
OPAQUE = {"errors"}

_seen: dict[tuple[str, str], str] = {}


def placeholder(key: str, value: str) -> str:
    """Устойчивая заглушка для значения. Разные значения одного ключа
    получают разные номера — иначе два проекта слились бы в один и
    фикстура перестала бы показывать, что их было два."""
    slot = _seen.setdefault((key, value), f"{key}-{len(_seen)}")
    return slot


def scrub(node, key=None, opaque=False):
    if isinstance(node, dict):
        return {k: scrub(v, k, opaque or k in OPAQUE) for k, v in node.items()}
    if isinstance(node, list):
        return [scrub(v, key, opaque) for v in node]
    if isinstance(node, str) and key in PERSONAL and node and not opaque:
        return placeholder(key, node)
    return node


DST.mkdir(parents=True, exist_ok=True)
for src in sorted(SRC.glob("*.json")):
    data = json.loads(src.read_text())
    (DST / src.name).write_text(
        json.dumps(scrub(data), ensure_ascii=False, indent=2) + "\n"
    )
    print(f"  {src.name}")
print(f"обезличено в {DST}")
