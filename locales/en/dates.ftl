# Day and month names, and how a date reads. $day is 0 for Monday to 6 for Sunday.
weekday-short = { $day ->
    [0] Mon
    [1] Tue
    [2] Wed
    [3] Thu
    [4] Fri
    [5] Sat
   *[6] Sun
}
weekday-long = { $day ->
    [0] Monday
    [1] Tuesday
    [2] Wednesday
    [3] Thursday
    [4] Friday
    [5] Saturday
   *[6] Sunday
}
month-short = { $month ->
    [1] Jan
    [2] Feb
    [3] Mar
    [4] Apr
    [5] May
    [6] Jun
    [7] Jul
    [8] Aug
    [9] Sep
    [10] Oct
    [11] Nov
   *[12] Dec
}
# "Tue, Oct 13"
date-day = { $weekday }, { $month } { $day }
# "Tuesday, Oct 13"
date-long-day = { $weekday }, { $month } { $day }
# "Oct 13"
date-month-day = { $month } { $day }
