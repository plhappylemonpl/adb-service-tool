//! Półautomatyczna kolejka Sklepu Google Play: otwiera stronę aplikacji
//! (`market://details?id=<pakiet>`), co ~2 s sprawdza, czy pakiet jest już
//! zainstalowany, albo czeka na "Dalej"/"Pomiń" (komputer lub agent), i przechodzi
//! do następnej. Kliknięcia "Zainstaluj" nie da się (i nie wolno) zautomatyzować.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ItemStatus {
    Pending,
    /// Strona aplikacji otwarta w Sklepie Play, czekamy na instalację.
    Opened,
    /// Wykryto instalację (pm path / PackageManager).
    Installed,
    /// Użytkownik kliknął "Dalej" bez wykrytej instalacji.
    Advanced,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub package: String,
    pub label: String,
    pub status: ItemStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PlayQueue {
    pub items: Vec<QueueItem>,
    /// Indeks bieżącej pozycji (== items.len() -> koniec).
    pub current: usize,
    /// Licznik zmian - agent/GUI odświeżają widok tylko po zmianie.
    pub rev: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Next,
    Skip,
}

impl PlayQueue {
    pub fn new(items: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            items: items
                .into_iter()
                .map(|(package, label)| QueueItem { package, label, status: ItemStatus::Pending })
                .collect(),
            current: 0,
            rev: 0,
        }
    }

    pub fn current(&self) -> Option<&QueueItem> {
        self.items.get(self.current)
    }

    pub fn finished(&self) -> bool {
        self.current >= self.items.len()
    }

    /// (pozycja 1-based, razem) - "3 / 18".
    pub fn position(&self) -> (usize, usize) {
        ((self.current + 1).min(self.items.len()), self.items.len())
    }

    /// Bieżąca pozycja do otwarcia w Sklepie Play (tylko raz na pozycję).
    pub fn take_to_open(&mut self) -> Option<String> {
        let it = self.items.get_mut(self.current)?;
        if it.status != ItemStatus::Pending {
            return None;
        }
        it.status = ItemStatus::Opened;
        self.rev += 1;
        Some(it.package.clone())
    }

    /// Wykryto instalację pakietu. Gdy to bieżąca pozycja - przejście dalej.
    /// Pozycje dalej w kolejce też są oznaczane (użytkownik mógł zainstalować wcześniej).
    pub fn mark_installed(&mut self, package: &str) -> bool {
        let Some(i) = self.items.iter().position(|x| x.package == package) else { return false };
        if matches!(self.items[i].status, ItemStatus::Installed | ItemStatus::Skipped | ItemStatus::Advanced) {
            return false;
        }
        self.items[i].status = ItemStatus::Installed;
        self.rev += 1;
        if i == self.current {
            self.advance();
        }
        true
    }

    pub fn act(&mut self, a: Action) -> bool {
        let Some(it) = self.items.get_mut(self.current) else { return false };
        it.status = match a {
            Action::Next => ItemStatus::Advanced,
            Action::Skip => ItemStatus::Skipped,
        };
        self.rev += 1;
        self.advance();
        true
    }

    /// Przejście do następnej pozycji, pomijając już zainstalowane.
    fn advance(&mut self) {
        self.current += 1;
        while let Some(it) = self.items.get(self.current) {
            if it.status == ItemStatus::Installed {
                self.current += 1;
            } else {
                break;
            }
        }
    }

    pub fn count(&self, s: ItemStatus) -> usize {
        self.items.iter().filter(|i| i.status == s).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(n: usize) -> PlayQueue {
        PlayQueue::new((1..=n).map(|i| (format!("com.app{i}"), format!("App {i}"))))
    }

    #[test]
    fn queue_flow() {
        let mut q = q(4);
        assert_eq!(q.position(), (1, 4));
        assert_eq!(q.take_to_open().as_deref(), Some("com.app1"));
        assert_eq!(q.take_to_open(), None, "otwierane tylko raz");
        assert!(q.mark_installed("com.app1"));
        assert_eq!(q.position(), (2, 4));
        assert_eq!(q.current().unwrap().package, "com.app2");
        // użytkownik zainstalował app3 wcześniej - zostanie przeskoczona
        assert!(q.mark_installed("com.app3"));
        assert_eq!(q.current().unwrap().package, "com.app2");
        assert_eq!(q.take_to_open().as_deref(), Some("com.app2"));
        assert!(q.act(Action::Skip));
        assert_eq!(q.current().unwrap().package, "com.app4");
        assert!(q.act(Action::Next));
        assert!(q.finished());
        assert!(!q.act(Action::Next));
        assert_eq!(q.position(), (4, 4));
        assert_eq!(q.count(ItemStatus::Installed), 2);
        assert_eq!(q.count(ItemStatus::Skipped), 1);
        assert_eq!(q.count(ItemStatus::Advanced), 1);
        assert!(!q.mark_installed("com.unknown"));
        assert!(!q.mark_installed("com.app2"), "pominięta pozycja nie wraca");
    }

    #[test]
    fn empty_and_serialized() {
        let mut e = PlayQueue::new(Vec::<(String, String)>::new());
        assert!(e.finished());
        assert_eq!(e.position(), (0, 0));
        assert_eq!(e.take_to_open(), None);
        let mut q = q(2);
        let r0 = q.rev;
        q.take_to_open();
        assert!(q.rev > r0);
        let v = serde_json::to_value(&q).unwrap();
        assert_eq!(v["items"][0]["status"], "opened");
        assert_eq!(v["current"], 0);
    }
}
