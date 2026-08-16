use std::time::{SystemTime, UNIX_EPOCH};

use wakode_core::Micros;

/// Текущее время в микросекундах от эпохи UTC.
///
/// Единственное место в крейте, где берутся системные часы. Вынесено
/// отдельным модулем, чтобы обращений к ним не расползлось: `received_at`,
/// `created_at` и отметки об использовании ключа должны браться из одного
/// источника, иначе в пределах одной транзакции появятся расходящиеся
/// значения «сейчас».
pub(crate) fn now() -> Micros {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Micros::new(since_epoch.as_micros() as i64)
}
