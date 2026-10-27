# Nazwy dni i miesięcy oraz zapis daty. $day to 0 dla poniedziałku do 6 dla niedzieli.
weekday-short = { $day ->
    [0] pon.
    [1] wt.
    [2] śr.
    [3] czw.
    [4] pt.
    [5] sob.
   *[6] niedz.
}
weekday-long = { $day ->
    [0] poniedziałek
    [1] wtorek
    [2] środa
    [3] czwartek
    [4] piątek
    [5] sobota
   *[6] niedziela
}
# Skróty w dopełniaczu, bo stoją po liczbie dnia: „13 paź”.
month-short = { $month ->
    [1] sty
    [2] lut
    [3] mar
    [4] kwi
    [5] maj
    [6] cze
    [7] lip
    [8] sie
    [9] wrz
    [10] paź
    [11] lis
   *[12] gru
}
# „wt., 13 paź”
date-day = { $weekday }, { $day } { $month }
# „wtorek, 13 paź”
date-long-day = { $weekday }, { $day } { $month }
# „13 paź”
date-month-day = { $day } { $month }
