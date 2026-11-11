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
# Ankieta, która się zamknęła i czeka na zatwierdzenie.
poll-waiting = Głosowanie zakończone. **{ $day }** ma najwięcej głosów i czeka na potwierdzenie.
poll-skipped = W tym tygodniu nie ma wydarzenia.
poll-expired = Nikt nie potwierdził terminu **{ $day }** na czas, więc w tym tygodniu nie ma wydarzenia.
# Prośba, którą krok zatwierdzenia wysyła na swój kanał.
approval-title = Potwierdź: { $title }
approval-ask = Ankieta wybrała **{ $day }** ({ $count } { $count ->
    [one] głos
    [few] głosy
   *[other] głosów
}). Opublikuj wydarzenie w tym dniu, wybierz inny dzień albo pomiń ten tydzień. Prośba wygasa { $when }.
approval-posted = { $name } potwierdza **{ $day }**. Wydarzenie jest opublikowane.
approval-skipped = { $name } pomija ten tydzień.
approval-expired = Nikt nie odpowiedział na czas, więc w tym tygodniu nie ma wydarzenia.
approval-skip = Pomiń ten tydzień
post-guests = + goście
