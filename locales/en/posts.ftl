# What Marian posts in channels: events, polls, reminders.
post-location = Location
post-time = Time
post-waiting = { $count } waiting
post-option-fallback = Option
post-edit = Edit
post-delete = Delete
post-created-by = Created by { $name }
post-event-fallback = Event
post-delete-title = Delete event
post-delete-posted = This removes the Discord post. The event is marked cancelled.
post-delete-draft = This deletes the event.
status-draft = Draft
status-scheduled = Scheduled
status-posted = Posted
status-archived = Archived
status-cancelled = Cancelled
status-skipped = Skipped
status-failed = Post failed
status-unknown = Unknown
# $start is a Discord timestamp that counts down, such as "in 15 minutes".
reminder-title = REMINDER: Event starting { $start }!
reminder-footer = Opt out of reminders with /reminders off
poll-next-title = Next event: { $title }
poll-results-title = Poll results: { $title }
# $when is a countdown, such as "in 3 hours", or a day and a time.
poll-locks = Locks { $when }.
poll-won = **{ $day }** won with { $count } { $count ->
    [one] vote
   *[other] votes
}. The event is in the post above.
# The name of the role Marian gives an event's attendees.
role-attendee = { $title } attendee
# A poll that closed and waits for an approval step.
poll-waiting = Voting has closed. **{ $day }** got the most votes and is waiting for confirmation.
poll-skipped = There's no event this week.
poll-expired = No one confirmed **{ $day }** in time, so there's no event this week.
# The request an approval step posts in its channel.
approval-title = Confirm { $title }
approval-ask = The poll picked **{ $day }** with { $count } { $count ->
    [one] vote
   *[other] votes
}. Post the event on that day, pick another day, or skip this week. This request lapses { $when }.
approval-posted = { $name } confirmed **{ $day }**. The event is posted.
approval-skipped = { $name } skipped this week.
approval-expired = No one answered in time, so there's no event this week.
approval-skip = Skip this week
post-guests = +X
