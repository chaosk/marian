# What Marian posts in channels: events, polls, reminders.
post-location = Location
post-time = Time
post-view-on-web = View on web
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
