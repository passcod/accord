# Accord

At the moment, Accord is only an idea. It may get implemented this year.

Accord is a Discord API client to power Discord API clients. Like bots. It is itself built on top of
whichever Discord API library I choose to build it. (Probably Serenity.) So, perhaps it should be
called a middleware.

Accord is about translating a specialised interface (Discord's API) to a very common interface (HTTP
calls to a server), and back.

One thing I find when writing Discord bots is that a lot of logic that is already reliably
implemented by other software a lot older than Discord itself has to regularly be reimplemented for
a bot's particular usecase... and I'd rather be writing business logic.

Another is that invariably whenever I start a Discord bot project I end up wanting to write some
parts of it in a different language or using a different stack. If I had Accord, I could.

So, in Accord, a typical interaction with a bot would go like this:

1. Someone invokes the bot, e.g. by saying `!roll d20`
2. Discord sends Accord the message via WebSocket
3. Accord makes a POST request to `http://localhost:1234/server/123/channel/456/message` with the
   message contents as the body, plus various bits of metadata in the headers
4. Your "bot" which is really a server listening on port 1234 accepts that request, processes it
   (rolls a d20) and returns the answer in the response body with code 200
5. Accord reads the response, sees it means to reply, adds in the channel and server/guild
   information if those weren't provided in the response headers
6. Accord posts a message to Discord containing the reply.

You don't need to have your bot listen on port 1234 itself. In fact, that is not recommended. What
you should do instead is run it behind nginx. Why? Let's answer with another few scenarios:

- What if the answer to a command is always the same?

  Instead of having an active server process and answer the same thing every time, write an nginx
  rule to match that request and reply with the contents of a static file on disk.

- What if the answer changes infrequently?

  Add a cache. This is built-in to nginx in a few different ways, or use Varnish or something.

- What if the answer is expensive, and/or you don't want it abused?

  Add rate limiting. This is built-in to nginx.

- What if you want to scale out the amount of backends?

  Scale horizontally, use nginx's round-robin upstream support.

- What if you want to partially scale *in*, for example because you serve lots of guilds and need to
  shard for your expensive endpoints, but your cheap endpoints are perfectly capable of handling the
  load?

  Point your sharded Accords to their own nginxes, and forward cheap requests to one backend server.

There are many more fairly common scenarios that, in usual Discord bots, would require a lot of
engineering, but with the Accord approach, **are already solved**.

Okay, but, that may work well for query-reply bots, but your bot needs to reply more than once, or
needs to post spontaneously, for example in response to an external event.

There are four approaches with Accord.

1. Do the call yourself. Have a Discord client in your app that calls out. Accord doesn't (and
   cannot) stop you from doing this.

2. Use the reverse interface. Accord exposes a server of its own, and you can make requests to that
   server to use Accord's Discord connection to make requests. Accord adds authentication to Discord
   on top, so you don't need to handle credentials in two places.

3. Summon a ghost. You can make a special call via the reverse interface mentioned above that will
   cause Accord to fire a request in the usual way, as if it was reacting to a message or other
   Discord action, but actually that message or action does not exist. In that way you can
   implement code all the same, and take advantage of the existing layering (cache etc).

4. In the special case of needing to answer multiple times in response to an event, you can respond
   using chunked output, keep the output stream alive with null bytes sent at 30–60s intervals, and
   send through multiple payloads separated by at least two null bytes. The payloads will be sent
   as soon as each one is received.

What if you need to stream some audio to a voice channel?

- You can stream audio, in whatever format Accord supports (and it will transcode on the fly if it's
  not something Discord supports), as the response.

- You can reply with a 302 redirect to a static audio file, and Accord will do the same (but it
  might be a little more clever in regards to buffering if it detects it can do range requests).
  You can even redirect to an external resource (not recommended, for security and performance
  reasons... but you can do it).

## Beyond Discord

This... is only the beginning.

Because Discord is one thing, but what if you had this same kind of gateway for Twitter? Matrix?
Zulip? IRC? Slack? Email? etc

All these have anywhere from slightly to majorly different models in how they operate, but you still
have a core mechanic of posting messages and expecting answers. You'll probably have subtilities and
adaptations, but what if you could reuse vast swathes of functionality by _rewriting some routes_?

The first example above, a bot that rolls a die, could be the exact same backend program served for
Slack and for Discord. At the same time, and in parallel, you could have a Discord-specific voice
endpoint, and a Slack-specific poll endpoint.

## Isn't this really inefficient?

Yeah, kinda. Instead of a bot that interacts directly with Discord, you have at least two additional
layers. All that adds is a few tens of milliseconds. What you _gain_ is likely worth it. By a lot.

## Why isn't this a thing yet?

~~Because I have too many other projects on the stove and I'm trying to cut back.~~

It's happening!

### Common `ACCORD_COMMAND_REGEX`

Use https://rustexp.lpil.uk to play around.

 - `(?:^~|\s+)(\w+)` matches `~command sub command` and routes to `.../command/sub/command...`
