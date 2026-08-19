#!/usr/bin/env bash
# Снять эталонные ответы с живого wakatime.com.
#
# Зачем: подмодуль `compat` обязан совпадать с чужим протоколом байт в
# байт, а документации на часть форм нет вовсе (`statusbar/today`).
# Единственный способ узнать, что мы совместимы, а не думаем так, —
# сравнивать с настоящими ответами.
#
# Второе назначение — калибровка `tail_padding_secs`. Сегодня она равна
# нулю, потому что величина добавки к последней отметке сессии нигде не
# описана. Решается арифметикой: `durations` показывает, как WakaTime сам
# нарезал день на интервалы, а `heartbeats` — из каких отметок он их
# собрал. Разница между суммой интервалов и суммой промежутков между
# отметками и есть искомая добавка.
#
# Использование:
#
#   export WAKATIME_API_KEY=waka_xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
#   tools/capture-wakatime-fixtures.sh                 # только чтение
#   tools/capture-wakatime-fixtures.sh --with-writes   # плюс запись отметки
#
# Ключ читается из переменной окружения, а не из аргумента: аргументы
# видны в `ps` любому процессу того же пользователя.

set -euo pipefail

: "${WAKATIME_API_KEY:?переменная WAKATIME_API_KEY не задана; возьмите ключ на https://wakatime.com/settings/api-key}"

OUT="${OUT:-fixtures/wakatime}"
WITH_WRITES=0
[ "${1:-}" = "--with-writes" ] && WITH_WRITES=1

# Дата, за которую снимаются посуточные ответы. По умолчанию вчера: у
# сегодняшнего дня цифры ещё едут, и снимок вчерашнего воспроизводим, а
# сегодняшнего — нет.
DAY="${DAY:-$(date -u -d 'yesterday' +%F)}"

AUTH="Authorization: Basic $(printf '%s' "$WAKATIME_API_KEY" | base64 -w0)"
BASE="https://wakatime.com/api/v1"

mkdir -p "$OUT"

# Каким стеком ходить.
#
# У wakatime.com есть и A, и AAAA, и на сети со сломанным маршрутом IPv6
# curl честно предпочтёт AAAA и обвалится на рукопожатии TLS с
# `unexpected eof while reading` — сообщение, по которому думаешь на
# блокировку или на ключ, а виноват маршрут. Пробуем как есть, и только
# если не вышло, прижимаемся к IPv4: жёсткий `--ipv4` сломал бы тех, у
# кого наоборот нет IPv4.
STACK=()
if ! curl --silent --show-error --max-time 15 --output /dev/null https://wakatime.com/api/v1 2>/dev/null; then
    if curl --ipv4 --silent --show-error --max-time 15 --output /dev/null https://wakatime.com/api/v1 2>/dev/null; then
        STACK=(--ipv4)
        printf 'IPv6 до wakatime.com не работает, иду по IPv4\n' >&2
    else
        printf 'wakatime.com не отвечает ни по IPv6, ни по IPv4 — дальше смысла нет\n' >&2
        exit 1
    fi
fi

# Что не снялось. Один отказавший эндпоинт не повод бросать остальные:
# снимок нужен целиком, а разбираться с недостающим проще, когда видно,
# что именно недостаёт.
FAILED=()

# Снять один ответ. Пишет тело и код состояния рядом: код — часть
# контракта не меньше тела, а `curl` по умолчанию его не сохраняет.
grab() {
    local name="$1" url="$2"
    local body="$OUT/$name.json" meta="$OUT/$name.status"

    local code
    if ! code=$(curl "${STACK[@]}" --silent --show-error --location --max-time 60 \
                     --header "$AUTH" \
                     --write-out '%{http_code}' \
                     --output "$body.raw" \
                     "$url"); then
        printf '  %-24s ОТКАЗ СЕТИ\n' "$name"
        FAILED+=("$name")
        rm -f "$body.raw"
        return 0
    fi
    printf '%s\n' "$code" > "$meta"

    # 401 и 403 — не сетевой сбой, а негодный ключ. Тело такого ответа
    # сохранять нельзя: оно непустое и в куче снимков прочиталось бы как
    # форма ответа эндпоинта.
    if [ "$code" = 401 ] || [ "$code" = 403 ]; then
        printf '  %-24s %s  ключ не подошёл\n' "$name" "$code"
        FAILED+=("$name")
        rm -f "$body.raw" "$meta"
        return 0
    fi

    # Форматирование, а не хранение как пришло: диф между нашим ответом и
    # эталоном должен читаться построчно. Порядок ключей сохраняется —
    # `jq` без `-S` его не трогает, а он тоже часть формы.
    if jq . < "$body.raw" > "$body" 2>/dev/null; then
        rm -f "$body.raw"
    else
        mv "$body.raw" "$body"
        printf '  ! тело не разобралось как JSON, сохранено как есть\n' >&2
    fi

    printf '  %-24s %s  %s\n' "$name" "$code" "$(wc -c < "$body") байт"
}

printf 'Снимаю за %s в %s\n' "$DAY" "$OUT"

# --- Формы ответов: то, что подмодуль `compat` обязан повторить ---------

grab current              "$BASE/users/current"
grab statusbar-today      "$BASE/users/current/statusbar/today"
grab all-time-since-today "$BASE/users/current/all_time_since_today"
grab summaries-one-day    "$BASE/users/current/summaries?start=$DAY&end=$DAY"

# Диапазон из нескольких дней — отдельным снимком: `cumulative_total` и
# `daily_average` на одном дне вырождаются и формы не показывают.
WEEK_AGO="$(date -u -d "$DAY - 6 days" +%F)"
grab summaries-week       "$BASE/users/current/summaries?start=$WEEK_AGO&end=$DAY"

# Диапазон, в котором заведомо есть пустой день. Спека требует, чтобы
# `summaries` отдавал пустые дни, которых разбиение по локальным дням не
# возвращает, — без такого снимка проверить это не с чем.
MONTH_AGO="$(date -u -d "$DAY - 29 days" +%F)"
grab summaries-month      "$BASE/users/current/summaries?start=$MONTH_AGO&end=$DAY"

# --- Калибровка tail_padding_secs --------------------------------------

grab heartbeats-day       "$BASE/users/current/heartbeats?date=$DAY"
grab durations-day        "$BASE/users/current/durations?date=$DAY"

# --- Запись: только по явному согласию ---------------------------------

if [ "$WITH_WRITES" = 1 ]; then
    printf '\nОтправляю пробные отметки — они появятся в вашей статистике.\n'

    NOW=$(date +%s)
    curl --silent --show-error --header "$AUTH" \
         --header 'Content-Type: application/json' \
         --data "{\"entity\":\"/tmp/wakode-fixture-probe.txt\",\"type\":\"file\",\"time\":$NOW}" \
         --write-out '%{http_code}\n' \
         --output "$OUT/heartbeat-single.json" \
         "$BASE/users/current/heartbeats" > "$OUT/heartbeat-single.status"

    curl --silent --show-error --header "$AUTH" \
         --header 'Content-Type: application/json' \
         --data "[{\"entity\":\"/tmp/wakode-fixture-probe.txt\",\"type\":\"file\",\"time\":$((NOW + 1))},{\"entity\":\"\",\"type\":\"file\",\"time\":$((NOW + 2))}]" \
         --write-out '%{http_code}\n' \
         --output "$OUT/heartbeat-bulk.json" \
         "$BASE/users/current/heartbeats.bulk" > "$OUT/heartbeat-bulk.status"

    # Вторая отметка в батче намеренно негодная (пустой `entity`):
    # интересна не только успешная форма, но и то, как WakaTime сообщает
    # об отказе одного элемента, не роняя весь запрос. Именно этим bulk и
    # отличается от одиночной отправки.
    for f in heartbeat-single heartbeat-bulk; do
        jq . < "$OUT/$f.json" > "$OUT/$f.json.fmt" 2>/dev/null && mv "$OUT/$f.json.fmt" "$OUT/$f.json"
        printf '  %-24s %s\n' "$f" "$(cat "$OUT/$f.status")"
    done
fi

# --- Что делать дальше -------------------------------------------------

if [ ${#FAILED[@]} -gt 0 ]; then
    printf '\nНе снялось: %s\n' "${FAILED[*]}"
    printf 'Остальное сохранено — разберитесь с этими и допрогоните.\n'
fi

cat <<EOF

Готово. Снято в $OUT.

ПРЕЖДЕ ЧЕМ КОММИТИТЬ — прочитайте файлы. В них ваши настоящие данные:
имена проектов и веток, пути к файлам, имена машин, часовой пояс, а в
$OUT/current.json — ещё и почта с логином. Что из этого не должно попасть
в публичный репозиторий, решаете вы; заменять значения можно свободно,
эталоном тут является форма, а не содержимое.

Быстро посмотреть, что там:

  jq 'keys' $OUT/*.json
  jq '.data | keys' $OUT/current.json
  jq '.data[0] | keys' $OUT/summaries-one-day.json
EOF
