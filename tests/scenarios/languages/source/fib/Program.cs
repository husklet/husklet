using System;

ulong a = 0;
ulong b = 1;
for (int index = 0; index < 50; index++)
{
    ulong next = a + b;
    a = b;
    b = next;
}
Console.WriteLine("NETFIB " + a);
