# Nix Binary Cache として使う

```nix
{
  nix.settings.extra-substituters = [
    "https://cache.example.com?trusted=true"
  ];
}
```
