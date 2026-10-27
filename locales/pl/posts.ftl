# Co Marian publikuje na kanałach: wydarzenia, ankiety, przypomnienia.
post-location = Miejsce
post-time = Czas
post-view-on-web = Zobacz w przeglądarce
post-waiting = { $count ->
    [one] { $count } osoba czeka
    [few] { $count } osoby czekają
   *[many] { $count } osób czeka
}
post-option-fallback = Opcja
post-edit = Edytuj
post-delete = Usuń
post-created-by = Utworzone przez { $name }
post-event-fallback = Wydarzenie
post-delete-title = Usuń wydarzenie
post-delete-posted = To usunie post na Discordzie. Wydarzenie zostanie oznaczone jako odwołane.
post-delete-draft = To usunie wydarzenie.
status-draft = Szkic
status-scheduled = Zaplanowane
status-posted = Opublikowane
status-archived = Zarchiwizowane
status-cancelled = Odwołane
status-skipped = Pominięte
status-failed = Publikacja nieudana
status-unknown = Nieznany
# $start to znacznik czasu Discorda, który odlicza, np. „za 15 minut”.
reminder-title = PRZYPOMNIENIE: wydarzenie zaczyna się { $start }!
reminder-footer = Wyłącz przypomnienia komendą /reminders off
poll-next-title = Następne wydarzenie: { $title }
poll-results-title = Wyniki ankiety: { $title }
# $when to odliczanie, np. „za 3 godziny”, albo dzień i godzina.
poll-locks = Koniec głosowania: { $when }.
poll-won = **{ $day }** wygrywa z { $count } { $count ->
    [one] głosem
   *[other] głosami
}. Wydarzenie jest w poście powyżej.
# Nazwa roli, którą Marian nadaje uczestnikom wydarzenia.
role-attendee = { $title } – uczestnicy
