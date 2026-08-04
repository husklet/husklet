SECTIONS
{
  .elfwrite : ALIGN(0x4000)
  {
    KEEP(*(.elfwrite))
    . = ALIGN(0x4000);
  }
}
INSERT AFTER .note.ABI-tag;
