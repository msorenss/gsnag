#!/usr/bin/env python3
"""Minimal StatusNotifierWatcher used only on a private test bus."""
import json
import sys
import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()
name = dbus.service.BusName('org.kde.StatusNotifierWatcher', bus)


class Watcher(dbus.service.Object):
    def __init__(self):
        super().__init__(bus, '/StatusNotifierWatcher')
        self.items = []

    @dbus.service.method('org.kde.StatusNotifierWatcher', in_signature='s', sender_keyword='sender')
    def RegisterStatusNotifierItem(self, service, sender=None):
        service = str(service)
        item = {'service': str(sender) if service.startswith('/') else service,
                'path': service if service.startswith('/') else '/StatusNotifierItem'}
        self.items.append(item['service'] + item['path'])
        with open(sys.argv[1], 'a') as output:
            output.write(json.dumps(item) + '\n')
        self.StatusNotifierItemRegistered(self.items[-1])

    @dbus.service.signal('org.kde.StatusNotifierWatcher', signature='s')
    def StatusNotifierItemRegistered(self, item):
        pass

    @dbus.service.method('org.freedesktop.DBus.Properties', in_signature='ss', out_signature='v')
    def Get(self, interface, prop):
        return self.GetAll(interface)[prop]

    @dbus.service.method('org.freedesktop.DBus.Properties', in_signature='s', out_signature='a{sv}')
    def GetAll(self, interface):
        return {'ProtocolVersion': dbus.Int32(0),
                'IsStatusNotifierHostRegistered': dbus.Boolean(True),
                'RegisteredStatusNotifierItems': dbus.Array(self.items, signature='s')}


watcher = Watcher()
GLib.MainLoop().run()
